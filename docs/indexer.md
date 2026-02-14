# Indexer Service Design (Draft)

This document describes the architecture of the Indexer service, which handles log ingestion, buffering, and flushing to S3.

## Architecture Overview

```
                              ┌─────────────────────────────────────────────────┐
                              │                  INDEXER SERVICE                │
                              │                                                 │
  HTTP Clients                │   ┌──────────────────────────────────────────┐  │
       │                      │   │              SHARED STATE                │  │
       │                      │   │                                          │  │
       ▼                      │   │  ┌─────────────┐    ┌─────────────────┐  │  │
  ┌─────────┐                 │   │  │ BufferMgr   │    │  CatalogCache   │  │  │
  │ POST    │─────────────────┼──▶│  │ (memtables) │    │ (ns validation) │  │  │
  │/v1/logs │  write handlers │   │  └──────┬──────┘    └────────┬────────┘  │  │
  │/{ns}    │  (many, async)  │   │         │                    │           │  │
  └─────────┘                 │   │         │    ┌───────────────┘           │  │
                              │   │         │    │                           │  │
  ┌─────────┐                 │   │         ▼    ▼                           │  │
  │ GET     │─────────────────┼──▶│  ┌─────────────────┐  ┌──────────────┐   │  │
  │/health  │                 │   │  │    Storage      │  │ CacheManager │   │  │
  └─────────┘                 │   │  │  (S3 client)    │  │ (unified)    │   │  │
                              │   │  └─────────────────┘  └──────────────┘   │  │
                              │   └──────────────────────────────────────────┘  │
                              │                      ▲                          │
                              │                      │                          │
                              │   ┌──────────────────┴───────────────────────┐  │
                              │   │           FLUSHER TASK (single)          │  │
                              │   │                                          │  │
                              │   │  loop {                                  │  │
                              │   │    sleep(1s)                             │  │
                              │   │    for (ns, reason) in ready_for_flush() │  │
                              │   │      segment = drain(ns)                 │  │
                              │   │      storage.put(segment)                │  │
                              │   │      manifest.append(segment_meta)       │  │
                              │   │  }                                       │  │
                              │   └──────────────────────────────────────────┘  │
                              └─────────────────────────────────────────────────┘
```

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| **Ingestion** |
| `POST` | `/v1/logs/{namespace}` | Ingest logs (returns 404 if namespace doesn't exist) |
| **Namespace Management** |
| `POST` | `/v1/namespaces` | Create namespace |
| `GET` | `/v1/namespaces` | List namespaces |
| `GET` | `/v1/namespaces/{name}` | Get namespace details |
| `DELETE` | `/v1/namespaces/{name}` | Delete namespace (soft delete, stops accepting logs) |
| **Operational** |
| `GET` | `/health` | Liveness |
| `GET` | `/ready` | Readiness |
| `GET` | `/metrics` | Prometheus metrics |

## Shared State

```rust
pub struct IndexerState {
    /// Per-namespace memtables (lock-free writes)
    pub buffers: Arc<BufferManager>,

    /// Namespace registry (validates ns exists before accepting logs)
    pub catalog: Arc<CatalogCache>,

    /// S3 client for writing segments
    pub storage: Arc<dyn ObjectStore>,

    /// Unified cache manager (manifest, catalog, bloom, block, segment)
    pub cache: Arc<CacheManager>,

    /// Metrics
    pub metrics: Arc<IndexerMetrics>,
}
```

## Threads/Tasks

| Task | Count | Purpose |
|------|-------|---------|
| HTTP handlers | N (async) | Accept logs, validate namespace, push to BufferManager |
| Flusher | 1 | Periodically drain ready buffers → S3 → update manifest |

## Parallel Flushes with Bounded Concurrency

Flushing involves network calls (S3 PUT). A single sequential flusher would mean slow writes to namespace A block namespaces B, C, D. Instead, we use parallel flushes with a semaphore to bound concurrency:

```
                          ┌────────────────────────────────────────┐
                          │            FLUSHER TASK                │
                          │                                        │
                          │   ready = buffers.ready_for_flush()    │
                          │                                        │
                          │         ┌───┬───┬───┬───┐              │
                          │         │ns1│ns2│ns3│ns4│ ...          │
                          │         └─┬─┴─┬─┴─┬─┴─┬─┘              │
                          │           │   │   │   │                │
                          └───────────┼───┼───┼───┼────────────────┘
                                      │   │   │   │
                                      ▼   ▼   ▼   ▼
                          ┌────────────────────────────────────────┐
                          │      SEMAPHORE (max_concurrent=16)     │
                          └────────────────────────────────────────┘
                                      │   │   │   │
                          ┌───────────┼───┼───┼───┼────────────────┐
                          │           ▼   ▼   ▼   ▼                │
                          │    ┌─────────────────────────┐         │
                          │    │   spawn flush_namespace │ (async) │
                          │    │   - drain(ns)           │         │
                          │    │   - storage.put()       │         │
                          │    │   - manifest.append()   │         │
                          │    └─────────────────────────┘         │
                          │              WORKER POOL               │
                          └────────────────────────────────────────┘
```

```rust
async fn flusher_loop(state: Arc<IndexerState>, max_concurrent: usize) {
    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        for (ns_id, _reason) in state.buffers.ready_for_flush() {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let state = state.clone();
            let ns_id = ns_id.clone();

            tokio::spawn(async move {
                if let Err(e) = flush_namespace(&state, &ns_id).await {
                    tracing::error!(ns = %ns_id, error = %e, "flush failed");
                }
                drop(permit); // Release slot
            });
        }
    }
}
```

This gives us:

- **Parallel I/O** - Multiple namespaces flush concurrently
- **Bounded concurrency** - Semaphore prevents overwhelming S3
- **Isolation** - One namespace's failure doesn't block others
- **Simple** - Still one coordinator task, just spawns async work

## Request/Response Shapes

```rust
// POST /v1/namespaces
#[derive(Deserialize)]
struct CreateNamespaceRequest {
    name: String,
    #[serde(default = "default_retention")]
    retention_days: u32,  // default: 7
}

#[derive(Serialize)]
struct NamespaceResponse {
    id: String,           // "ns_01ABC..."
    name: String,
    retention_days: u32,
    created_at: DateTime<Utc>,
}

// POST /v1/logs/{namespace}
#[derive(Deserialize)]
struct IngestRequest {
    logs: Vec<LogLine>,
}

#[derive(Deserialize)]
struct LogLine {
    timestamp: Option<i64>,  // epoch ms, defaults to server time
    payload: serde_json::Value,
}

#[derive(Serialize)]
struct IngestResponse {
    accepted: usize,
}
```

## Namespace Creation Flow

```
POST /v1/namespaces { "name": "api-gateway", "retention_days": 14 }
    │
    ▼
┌──────────────────────────────────────────────────────────┐
│ 1. Validate name (alphanumeric, hyphens, underscores)    │
│ 2. Check catalog cache - name already exists? → 409     │
│ 3. Create NamespaceInfo with ULID                        │
│ 4. Write log entry to S3: /metadata/log/{ulid}.json     │
│ 5. Update head.json (append to log_tail)                 │
│ 6. Invalidate + refresh catalog cache                    │
│ 7. Return 201 with namespace details                     │
└──────────────────────────────────────────────────────────┘
```

## Log Ingestion Flow

```
POST /v1/logs/api-gateway { logs: [...] }
    │
    ▼
┌──────────────────────────────────────────────────────────┐
│ 1. Lookup namespace in catalog cache                     │
│    - Not found? → 404 "namespace not found"              │
│ 2. Validate payload sizes                                │
│ 3. buffers.add_batch(ns_id, entries)                     │
│ 4. Return 202 { accepted: N }                            │
│                                                          │
│ (Flush happens async in background)                      │
└──────────────────────────────────────────────────────────┘
```

## Design Decisions

### Backpressure: Reject + fly-replay

When buffers exceed a memory threshold, the indexer rejects requests and lets
the Fly proxy redirect the client to another instance.

```
if buffers.memory_usage() > SOFT_LIMIT:
    return 503 + fly-replay: elsewhere=true  // "go ask someone else"

if buffers.memory_usage() > HARD_LIMIT:
    return 503 + Retry-After header           // "everyone is full, back off"
```

The [`fly-replay`](https://fly.io/docs/networking/fly-replay/) header tells the
Fly proxy to replay the request on a different machine. Setting `elsewhere=true`
excludes the responding machine from the next load-balance decision, so the
client never even sees the 503 — the proxy transparently retries.

**Mitigations to keep in mind:**

- **Thundering herd on recovery** - Stagger `Retry-After` values to spread
  reconnections. Start rejecting early (at ~70-80% capacity) so there's headroom
  when redirected traffic arrives.
- **Cascade failure** - If all indexers are near capacity, `fly-replay` becomes
  a hot-potato loop. The `HARD_LIMIT` path with `Retry-After` is the backstop:
  it tells clients to genuinely back off instead of bouncing between machines.
- **Observability** - Track rejection rate per namespace to catch hot namespaces
  that consistently overflow a single indexer.

Note: `fly-replay` has a 1MB request size limit. Ingestion payloads should
stay well under this; if they don't, that's a client-side problem.

### Manifest and Catalog: Use CacheManager

No separate `ManifestManager` needed. The existing `CacheManager` already
provides `ManifestCache` (per-namespace, ETag-validated) and `CatalogCache`
(global namespace registry). The flusher reads from the manifest cache, does a
read-modify-write to S3, then invalidates the cache entry via
`cache.invalidate_namespace()`.

### Catalog Refresh: On Cache Miss

No background polling task. The catalog cache is refreshed on demand:

- **Ingestion request arrives** → look up namespace in `CatalogCache`.
- **Cache hit** → proceed.
- **Cache miss** → fetch catalog from S3, populate cache, then proceed.
- **Namespace not found after refresh** → 404.

This keeps the background task count to one (the flusher) and avoids wasted
polling when traffic is low.

**Mitigations:**

- **Cold start stampede** - If the indexer restarts with an empty cache and gets
  traffic for many namespaces at once, they all trigger parallel catalog fetches.
  Single-flight the refresh (one in-flight fetch at a time, waiters share the
  result) to collapse these into one S3 read.
- **Deleted namespaces** - A namespace deleted from the catalog may still be
  cached as valid, so the indexer keeps accepting logs for it until the cache
  entry is evicted or expires. This is acceptable: logs written to a deleted
  namespace can be garbage-collected later. If tighter consistency is needed,
  add a short TTL to catalog entries.
