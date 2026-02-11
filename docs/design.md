# Technical Design Document: Sybil

## **Version:** 0.1
## **Status:** Draft
## **Date:** 2025-02-08
## **Author:** Rugwiro Mbanda Valentin

---

### **1. Overview & Goals**

#### **1.1 Problem Statement**

Existing log storage solutions force a tradeoff:
- **Quickwit**: S3-native storage, but proprietary query language (learning curve, vendor lock-in)
- **ClickHouse-based (Logchef)**: SQL interface, but operational complexity (Postgres + ClickHouse cluster + auxiliary services)

Neither offers: **SQL + S3-native + operationally simple + schema-flexible**.

#### **1.2 Goals**

1. **SQL query interface** — universal, no new DSL to learn
2. **S3 as single source of truth** — stateless compute, minimal operational burden
3. **Schema flexibility** — JSON logs with auto-indexed fields for users following the standard schema
4. **Multi-tenant via namespaces** — a namespace can be an app, service, or tenant (S3 prefix isolation)
5. **Query-first performance** — optimized for fast reads during incident response
6. **Always queryable** — data is searchable immediately after ingestion with primary indexes; secondary indexes (full-text) built async by Steward

#### **1.3 Target Scale**

- 100s of GB/day aggregate ingestion
- 7-day default retention, namespace-configurable

#### **1.4 Performance Targets**

| Metric | Target | Notes |
|--------|--------|-------|
| **Query latency (hot)** | <50ms p99 | NVMe-cached segments |
| **Query latency (cold)** | <100ms p99 | S3 fetch required |
| **Query latency (unindexed)** | Best-effort | Exhaustive scan of log segments |
| **Ingestion-to-queryable** | ≤10 seconds | Time-based flush ensures bounded latency |

#### **1.5 SLA Priority**

1. Query latency
2. Ingestion latency
3. Uptime
4. Durability
5. Availability

#### **1.6 Standard Log Schema**

Users who follow this schema get automatic field indexing:

```json
{
  "timestamp": "2025-01-15T10:30:00.123Z",
  "level": "error",
  "service": "api-gateway",
  "message": "Connection timeout",
  ...
}
```

**Auto-indexed fields:**

| Field | Aliases | Description |
|-------|---------|-------------|
| `timestamp` | — | ISO 8601 timestamp, required |
| `level` | `severity` | Log level (error, warn, info, debug, trace) |
| `service` | `app`, `source` | Origin of the log |
| `component` | — | Component within a service |
| `namespace` | — | Logical grouping (k8s namespace, etc.) |
| `environment` | `env` | Deployment environment (prod, staging, dev) |
| `host` | — | Host/machine identifier |
| `trace_id` | — | Distributed trace correlation ID |

Additional fields (including `message`) are stored in the JSON payload and queried via segment scan.

#### **1.7 Non-Goals (v1)**

- Real-time streaming
- Built-in alerting
- Distributed tracing correlation

---

### **2. High-Level Architecture**

The system consists of three stateless, independently scalable components that use S3-compatible object storage (Tigris) as the single source of truth.

```
                    ┌─────────────┐
                    │   Clients   │
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
              ▼            │            ▼
        ┌──────────┐       │      ┌──────────┐
        │  Indexer │       │      │ Querier  │
        └────┬─────┘       │      └────┬─────┘
             │             │           │
             ▼             │           ▼
        ┌─────────────────────────────────┐
        │         Tigris (S3)             │
        │  ┌──────────-─┐  ┌────────────┐ │
        │  │Log Segments│  │  Indexes   │ │
        │  └──────────-─┘  └────────────┘ │
        └─────────────────────────────────┘
                           ▲
                           │
                    ┌──────┴──────┐
                    │  Steward    │
                    └─────────────┘
```

**Data Flow:**

1. **Indexer** — Accepts logs via HTTP, extracts indexed fields, builds bloom filters, and writes L0 segments to S3. Data is immediately queryable.
2. **Steward** — Background housekeeping: compacts segments (L0→L1→L2), builds secondary indexes (full-text search on `message` field), garbage collection.
3. **Querier** — Executes SQL queries. Uses primary indexes and bloom filters for fast lookups, falls back to segment scan when needed.

#### **2.1 Key Architectural Decisions**

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Primary storage | Tigris (S3-compatible) | Low-latency small objects, durability, cost-effective |
| Namespace isolation | S3 prefixes | Simple, sufficient isolation without bucket overhead |
| Indexing model | Synchronous (primary), Async (secondary) | Primary indexes built on ingestion; secondary indexes (full-text) built by Steward |
| Component coordination | Tigris notifications | Native support, proven in hedwig, avoids polling overhead |
| Querier caching | NVMe from day one | Critical for ≤50ms query latency target |
| Query routing | Consistent hashing by namespace | Ensures cache locality |

#### **2.2 Indexer Design**

**Batching strategy:** Memtable → flush to S3

- Incoming logs buffered in an in-memory memtable (sorted by timestamp)
- Flush triggered by size threshold OR time window
- Target segment size: ≤64KB — Tigris inlines objects up to 64KB (confirmed: "anything smaller than the memory space of the Commodore 64"). Larger objects bypass the inline optimization.
- S3 bandwidth is expensive; batching amortizes cost

**Indexing on write:** Auto-indexed fields are extracted, dictionary-encoded, and bloom filters built during segment creation. No async indexing step needed.

**No WAL:** Logs can tolerate some loss on crash. Optimize for throughput over durability.

#### **2.3 Steward Design**

A single, long-running coordinator that handles background housekeeping. See **Section 7** for detailed design.

**Triggered by:** Tigris object notifications (reactive) + internal heartbeat timer (scheduled)

**Responsibilities:**
- **Compaction** — Merge L0 segments into L1 (hourly), L1 into L2 (daily)
- **Secondary indexes** — Build full-text inverted indexes on `message` field
- **Garbage collection** — Clean up superseded segments, orphan files
- **Retention enforcement** — Delete segments beyond namespace retention period

**Architecture:** Single Steward instance with a pool of Tokio task workers. No coordination service needed—manifest ETag provides atomic updates. Scales to multiple Stewards via consistent hashing when needed.

#### **2.4 Querier Design**

**Query execution modes:**
- **Indexed path (■)** — uses inverted indexes, ≤50ms p50 target
- **Unindexed path (◈)** — exhaustive scan of recent log segments, best-effort latency

**Caching:** Local NVMe SSD cache (like turbopuffer)
- Namespace data cached after first access
- Consistent hashing routes queries by namespace for cache locality

#### **2.5 Open Questions**

1. **Optimal segment size** — Benchmark Tigris to find sweet spot for inlining benefits vs. write amplification.
2. **Flush window size** — What time threshold balances latency vs. batching efficiency?

---

### **3. Detailed Design: Storage Layer**

The storage layer uses a custom LSM-inspired segment format optimized for S3-native access patterns. Segments are immutable once written, with compaction merging smaller segments into larger ones.

#### **3.1 Performance Targets**

| Tier | Storage     | P99 Target |
|------|-------------|------------|
| Hot  | NVMe cache  | <50ms      |
| Cold | S3 | <100ms |            |

#### **3.2 Auto-Indexed Fields**

These fields are automatically extracted and indexed at ingestion:

```rust
const AUTO_INDEX_FIELDS: &[&str] = &[
    "service",
    "app",        // alias for service
    "source",     // alias for service
    "component",
    "namespace",
    "environment",
    "env",        // alias for environment
    "host",
    "level",
    "severity",   // alias for level
    "trace_id",
];
```

#### **3.3 Segment Hierarchy**

Segments are organized into three levels, inspired by LSM trees:

| Level | Size | Structure | Dictionary | Purpose |
|-------|------|-----------|------------|---------|
| L0 | ≤64KB | Flat (no blocks) | Per-segment | Fresh writes, Tigris-inlined |
| L1 | ~32MB | 64KB blocks | Per-block | Hourly compacted |
| L2 | ~256MB | 64KB blocks | Per-block | Daily compacted, archival |

**Namespace ID format:**

```
ns_{ulid}

Example: ns_01JQXYZ123ABCDEFGHJKMNPQRS
```

- `ns_` prefix: Recognizable as namespace ID
- ULID suffix: Time-sortable, alphanumeric (Crockford Base32)
- Total: 29 characters

**Object key structure:**

```
/namespaces/{namespace_id}/segments/L0/{hour}/{ulid}.seg
/namespaces/{namespace_id}/segments/L1/{date}/{hour}.seg
/namespaces/{namespace_id}/segments/L2/{date}.seg

Examples:
/namespaces/ns_01JQXYZ123ABCDEFGHJKMNPQRS/segments/L0/2025-01-15T14/01JQXYZ456.seg
/namespaces/ns_01JQXYZ123ABCDEFGHJKMNPQRS/segments/L1/2025-01-15/14.seg
/namespaces/ns_01JQXYZ123ABCDEFGHJKMNPQRS/segments/L2/2025-01-15.seg
```

All data for a namespace lives under `/namespaces/{namespace_id}/`, making it easy to list, isolate, or delete.

#### **3.4 L0 Segment Format**

L0 segments are small (≤64KB) to benefit from Tigris object inlining. No internal block structure—the entire segment is the unit of I/O.

```
┌────────────────────────────────────────┐
│ Log Entries (zstd compressed)          │
│ ┌────────────────────────────────────┐ │
│ │ Entry 0                            │ │
│ │   timestamp_delta: i32 LE          │ │
│ │   presence_bitmap: u16 LE          │ │
│ │   dict_ids: [u32 LE] × popcount    │ │
│ │   payload_len: u32 LE              │ │
│ │   payload: [u8] × payload_len      │ │
│ ├────────────────────────────────────┤ │
│ │ Entry 1 ... Entry N                │ │
│ └────────────────────────────────────┘ │
├────────────────────────────────────────┤
│ Dictionary                             │
│   entry_count: u32 LE                  │
│   For each entry:                      │
│     len: u16 LE                        │
│     value: [u8] × len                  │
├────────────────────────────────────────┤
│ Bloom Filters (one per indexed field)  │
│   filter_count: u8                     │
│   For each filter:                     │
│     field_id: u8                       │
│     len: u16 LE                        │
│     bits: [u8] × len                   │
├────────────────────────────────────────┤
│ Footer (40 bytes, fixed)               │
│   log_count: u32                       │
│   min_timestamp: i64                   │
│   max_timestamp: i64                   │
│   entries_size: u32                    │
│   dictionary_offset: u32               │
│   bloom_offset: u32                    │
│   checksum: u32 (CRC32C)               │
│   magic: u32 (0x5359424C = "SYBL")     │
└────────────────────────────────────────┘
```

**Read sequence:** Fetch footer (last 40 bytes) → validate magic/checksum → fetch dictionary → fetch bloom filters → check blooms → fetch and scan entries if needed.

**For L0 segments:** Since they're small (≤64KB), fetch the entire segment in one S3 GET request.

#### **3.5 L1/L2 Segment Format**

Larger segments use 64KB blocks for selective reads:

```
┌────────────────────────────────────────┐
│ Block 0 (64KB)                         │
│ ┌────────────────────────────────────┐ │
│ │ Entries (compressed)               │ │
│ ├────────────────────────────────────┤ │
│ │ Dictionary                         │ │
│ ├────────────────────────────────────┤ │
│ │ Block Footer                       │ │
│ │   log_count: u32                   │ │
│ │   min_timestamp: i64               │ │
│ │   max_timestamp: i64               │ │
│ │   checksum: u32                    │ │
│ └────────────────────────────────────┘ │
├────────────────────────────────────────┤
│ Block 1 ... Block N                    │
├────────────────────────────────────────┤
│ Block Index                            │
│   [min_ts, max_ts, offset, size] × N   │
├────────────────────────────────────────┤
│ Bloom Filters (per-field per-segment)  │
├────────────────────────────────────────┤
│ Segment Footer                         │
│   block_count: u32                     │
│   log_count: u64                       │
│   min_timestamp: i64                   │
│   max_timestamp: i64                   │
│   block_index_offset: u64              │
│   bloom_offset: u64                    │
│   checksum: u32                        │
│   magic: u32                           │
└────────────────────────────────────────┘
```

**Read sequence:** Fetch footer → fetch block index → use timestamps to identify relevant blocks → fetch only those blocks (parallel S3 range requests).

#### **3.6 Entry Encoding**

Each log entry within a segment:

```
┌────────────────────────────────────────┐
│ timestamp_delta: i32 LE                │  Delta from segment/block min_timestamp
├────────────────────────────────────────┤
│ presence_bitmap: u16 LE                │  Which auto-indexed fields exist
├────────────────────────────────────────┤
│ dict_ids: [u32 LE] × popcount(bitmap)  │  Dictionary IDs for present fields
├────────────────────────────────────────┤
│ payload_len: u32 LE                    │  Length of payload in bytes
├────────────────────────────────────────┤
│ payload: [u8] × payload_len            │  Original JSON log line
└────────────────────────────────────────┘
```

**Timestamp delta:** Signed i32 handles deltas of ±24 days in milliseconds, far exceeding any segment's time span. Negative deltas accommodate out-of-order logs.

**Dictionary encoding:** Field values are deduplicated within a segment/block. Entries store u32 IDs instead of repeated strings.

**Presence bitmap:** Bit N indicates whether auto-indexed field N exists. The number of dict_ids equals `popcount(bitmap)`.

#### **3.7 Byte Order**

All multi-byte integers use **little-endian** (LE) encoding. This matches the native byte order of x86/x64 and ARM, avoiding byte swapping overhead.

Length-prefixed fields follow the pattern `[len][data]` where length precedes the variable-length data it describes.

#### **3.8 Write Path**

```
JSON logs → In-memory buffer → Build segment → S3 PUT → Update catalog
```

**Flush triggers:**
- Estimated segment size ≥ 48KB (leaves headroom for dictionary, blooms, footer to stay ≤64KB), OR
- Oldest log in buffer > 10 seconds

**Durability:** No WAL. Accept loss of buffered logs on crash. Logs are best-effort; high-stake events typically repeat.

**Undersized segments:** Accepted when time-based flush triggers before size threshold. Compaction merges them.

#### **3.9 Compaction**

**Strategy:**
- L0 → L1: **Lazy** — triggered when L0 segment count for an hour exceeds threshold (e.g., 10)
- L1 → L2: **Eager** — triggered at day boundary

**Safe compaction protocol:**

```
Phase 1: BUILD
  1. Read L0 segments from S3
  2. Merge, sort by timestamp
  3. Write L1 segment to S3

  Crash here? L1 is orphan. Garbage collect later.

Phase 2: CATALOG SWITCH (atomic)
  4. Update catalog:
     - Add L1 segment
     - Mark L0 segments as SUPERSEDED

  Queries now see L1, ignore superseded L0s.

Phase 3: CLEANUP (async)
  5. Wait for TTL (60 seconds)
  6. Delete L0 segments from S3
  7. Remove L0 entries from catalog
```

**Segment states:**
- `ACTIVE` — Visible to queries
- `SUPERSEDED` — Replaced by higher-level segment, invisible to queries, awaiting cleanup

**Orphan handling:** During compaction, compare S3 listing to catalog. Delete orphan segments older than 5 minutes.

#### **3.10 Read Path**

```
Query → Catalog lookup → Bloom filter check → Fetch segments/blocks → Decompress → Filter → Stream results
```

**L0 reads:** Single S3 GET for entire segment (small enough).

**L1/L2 reads:**
1. Fetch footer (range request)
2. Fetch block index
3. Identify blocks matching time range
4. Fetch blocks in parallel (range requests)

**Streaming:** Results streamed as segments are processed. NVMe cache layer between S3 and query engine.

#### **3.11 Caching**

**NVMe cache layer:**

```
Query → Check NVMe cache → Hit: read from cache
                        → Miss: S3 fetch → write to cache → continue
```

- Cache key: segment path
- Eviction: LRU with time-based bias (recent segments stickier)
- Consistency: S3 is source of truth; cache is read-through only

#### **3.12 Manifest**

The manifest tracks all segments for a namespace. It uses an append-only log design for S3-native atomicity.

**Key structure:**

```
/namespaces/{namespace_id}/
  ├── manifest/
  │   ├── head.json                        # Current state pointer
  │   ├── snapshot-{timestamp}.json        # Compacted state
  │   └── log/
  │       └── {ulid}.json                  # Individual updates
  └── segments/
      ├── L0/{hour}/{ulid}.seg
      ├── L1/{date}/{hour}.seg
      └── L2/{date}.seg

Example: /namespaces/ns_01JQXYZ123ABCDEFGHJKMNPQRS/manifest/head.json
```

**Why append-only log:**
- Every write is a single atomic S3 PUT
- No read-modify-write races on log entries
- Immutable files are highly cacheable
- Similar to our segment compaction model

**Head.json:**

```json
{
  "version": 1,
  "snapshot": "snapshot-2025-01-15T00:00:00Z.json",
  "log_tail": ["01JQXYZ1", "01JQXYZ2", "01JQXYZ3"]
}
```

The head pointer is the only mutable component. It points to the current snapshot and lists log entry IDs to apply on top.

**Snapshot format:**

```json
{
  "created_at": "2025-01-15T00:00:00Z",
  "segments": [
    {
      "key": "namespaces/ns_01JQXYZ.../segments/L0/2025-01-15T14/01JQX.seg",
      "level": 0,
      "min_ts": 1705323600000,
      "max_ts": 1705323660000,
      "log_count": 4000,
      "size_bytes": 65536,
      "state": "active",
      "created_at": 1705323660000
    }
  ]
}
```

**Log entry format:**

```json
{
  "id": "01JQXYZ123",
  "timestamp": 1705323700000,
  "actions": [
    {
      "type": "add",
      "segment": {
        "key": "namespaces/ns_01JQXYZ.../segments/L1/2025-01-15/14.seg",
        "level": 1,
        "min_ts": 1705323600000,
        "max_ts": 1705327200000,
        "log_count": 100000,
        "size_bytes": 33554432,
        "state": "active",
        "created_at": 1705327200000
      }
    },
    {"type": "supersede", "key": "namespaces/ns_01JQXYZ.../segments/L0/2025-01-15T14/01JQX.seg"},
    {"type": "supersede", "key": "namespaces/ns_01JQXYZ.../segments/L0/2025-01-15T14/01JQY.seg"}
  ]
}
```

A single log entry can contain multiple actions, enabling atomic "add L1 + supersede L0s" during compaction.

**Read flow:**

```
1. GET head.json
2. GET snapshot (if not cached or changed)
3. GET log entries listed in head.log_tail (if not cached)
4. Apply log entries to snapshot → current segment list
5. Filter by time range, return active segments
```

**Write flow with ETag validation:**

S3 ETags enable optimistic concurrency control. When updating head.json:

```
1. PUT log entry to /manifest/log/{ulid}.json
   └── Atomic, no conflicts (unique key)

2. GET head.json
   └── Response includes ETag header (e.g., "a1b2c3d4")

3. Modify head in memory:
   └── Append new log entry ID to log_tail

4. PUT head.json with If-Match header:
   └── PUT /manifest/head.json
       If-Match: "a1b2c3d4"
       Body: {updated head content}

5. S3 checks ETag:
   └── Match → PUT succeeds (200 OK)
   └── No match → PUT rejected (412 Precondition Failed)
       └── Someone else updated head.json concurrently
       └── Retry from step 2
```

This gives us optimistic locking without external coordination. Conflicts are rare (only when multiple writers update the same namespace simultaneously) and automatically resolved via retry.

```rust
// Pseudocode
fn update_head(namespace: &str, new_log_id: &str) -> Result<()> {
    loop {
        let (head, etag) = s3.get_with_etag(head_key)?;

        let mut updated = head.clone();
        updated.log_tail.push(new_log_id);

        match s3.put_if_match(head_key, &updated, &etag) {
            Ok(_) => return Ok(()),
            Err(PreconditionFailed) => continue,  // Retry
            Err(e) => return Err(e),
        }
    }
}
```

**Manifest compaction (log → snapshot):**

When log grows large (e.g., >50 entries):

```
1. Read current snapshot + all log entries
2. Apply log entries → new state
3. PUT new snapshot file (new key, no conflict)
4. PUT new head.json with If-Match:
   └── Points to new snapshot
   └── Empty log_tail
5. DELETE old log entry files
6. DELETE old snapshot (keep 1-2 for recovery)
```

**Cache strategy:**

| Component | Mutability | Cache Strategy |
|-----------|------------|----------------|
| Snapshot | Immutable | Cache forever until new snapshot |
| Log entries | Immutable | Cache forever |
| Head.json | Mutable | ETag validation on every read |

**Warm cache read pattern:**

```
1. GET head.json with If-None-Match: {cached_etag}
   └── 304 Not Modified → use cached head, snapshot, log entries
   └── 200 OK → new head, check if snapshot/log entries changed
```

Typical warm read: **1 S3 request** (head.json ETag check returning 304).

**Segment states:**

| State | Meaning | Query Visibility |
|-------|---------|------------------|
| `active` | Current, valid segment | Visible |
| `superseded` | Replaced by compaction | Invisible |

Superseded segments remain in manifest until cleanup (TTL-based deletion from S3 + manifest update).

#### **3.13 Compression**

**Codec:** Zstd (excellent compression ratio for text/logs).

**Scope:**
- L0: Entire entries section compressed as one unit
- L1/L2: Each block compressed independently (enables selective decompression)

---

### **4. Detailed Design: Indexer**

The indexer accepts logs via HTTP, extracts indexed fields, buffers in memory, and flushes to S3 as L0 segments with primary indexes and bloom filters.

#### **4.1 HTTP API**

**Namespace management:**

```
POST /v1/namespaces
Content-Type: application/json

{
  "name": "Production API Gateway"
}

Response 201 Created:
{
  "id": "ns_01JQXYZ123ABCDEFGHJKMNPQRS",
  "name": "Production API Gateway",
  "retention_days": 7,
  "created_at": "2025-01-15T00:00:00Z"
}
```

**Log ingestion:**

```
POST /v1/namespaces/{namespace_id}/logs
Content-Type: application/json | application/x-ndjson
```

| Format | Content-Type | Body |
|--------|--------------|------|
| JSON array | `application/json` | `[{log1}, {log2}, ...]` |
| JSONL | `application/x-ndjson` | `{log1}\n{log2}\n...` |

**Example:**

```bash
curl -X POST https://sybil.example.com/v1/namespaces/ns_01JQXYZ.../logs \
  -H "Content-Type: application/x-ndjson" \
  -d '{"timestamp":"2025-01-15T14:00:00Z","level":"info","service":"api","message":"Request received"}
{"timestamp":"2025-01-15T14:00:01Z","level":"error","service":"api","message":"DB timeout"}'
```

**Successful response (202 Accepted):**

```
HTTP/1.1 202 Accepted
Content-Type: application/json

{
  "accepted": 2,
  "failed": 0
}
```

**Partial success (202 Accepted):**

```
HTTP/1.1 202 Accepted
Content-Type: application/json

{
  "accepted": 1,
  "failed": 1,
  "errors": [
    {"index": 1, "error": "missing required field: timestamp"}
  ]
}
```

202 Accepted indicates logs were received and buffered. Persistence to S3 happens asynchronously.

**Error responses:**

| Status | Condition |
|--------|-----------|
| 400 Bad Request | Invalid JSON, unparseable body |
| 404 Not Found | Namespace doesn't exist |
| 413 Payload Too Large | Request body > 10MB or > 100,000 logs |
| 503 Service Unavailable | Backpressure, system overloaded |

#### **4.2 Request Limits**

| Limit | Value | Rationale |
|-------|-------|-----------|
| Max request body | 10MB | Memory safety, reasonable for logs |
| Max logs per request | 100,000 | Safety limit |
| Max log line size | 64KB | Single log shouldn't exceed L0 segment |

Large batches that exceed segment size trigger immediate flush and continue processing.

#### **4.3 Timestamp Handling**

**Accepted formats:**

| Format | Example |
|--------|---------|
| ISO8601 | `"2025-01-15T14:00:00.123Z"` |
| Epoch milliseconds | `1705323600123` |

**Validation:**

```rust
fn validate_timestamp(ts: i64, now: i64, retention_days: u32) -> Result<()> {
    let max_future = now + Duration::from_hours(1).as_millis();
    let max_past = now - Duration::from_days(retention_days).as_millis();

    if ts > max_future {
        return Err("timestamp too far in future (>1 hour)");
    }
    if ts < max_past {
        return Err("timestamp exceeds retention period");
    }
    Ok(())
}
```

Logs with invalid timestamps are rejected individually; valid logs in the same batch are still accepted.

#### **4.4 Processing Pipeline**

```
HTTP Request
     │
     ▼
┌─────────────────┐
│ Validate size   │ ─── Too large → 413
└─────────────────┘
     │
     ▼
┌─────────────────┐
│ Check namespace │ ─── Not found → 404
│ exists (catalog)│
└─────────────────┘
     │
     ▼
┌─────────────────┐
│ Parse logs      │ ─── Invalid JSON → 400
│ (JSON/JSONL)    │
└─────────────────┘
     │
     ▼
┌─────────────────┐
│ For each log:   │
│ - Parse ts      │ ─── Invalid → skip, record error
│ - Validate ts   │ ─── Out of bounds → skip, record error
│ - Extract fields│
└─────────────────┘
     │
     ▼
┌─────────────────┐
│ Append to       │
│ namespace buffer│
└─────────────────┘
     │
     ▼
┌─────────────────┐
│ Check flush     │ ─── Triggers met → flush to S3
│ triggers        │
└─────────────────┘
     │
     ▼
   Return 202 Accepted

#### **4.5 Buffering**

**Per-namespace buffer:**

```rust
struct NamespaceBuffer {
    namespace_id: String,
    entries: Vec<ProcessedEntry>,
    dictionary: Dictionary,
    oldest_entry_time: Instant,     // For time-based flush
    estimated_compressed_size: usize,
}

struct ProcessedEntry {
    timestamp: i64,
    presence_bitmap: u16,
    indexed_values: Vec<String>,
    payload: Bytes,                 // Original JSON
}
```

**Buffer management:**

| Aspect | Design |
|--------|--------|
| Isolation | One buffer per namespace |
| Concurrency | Mutex per buffer, no cross-namespace contention |
| Memory limit | Max total buffers in memory, LRU eviction of idle namespaces |

#### **4.6 Flush Triggers**

```rust
fn should_flush(buffer: &NamespaceBuffer) -> bool {
    buffer.estimated_segment_size >= 48 * 1024              // 48KB threshold (≤64KB final segment)
        || buffer.oldest_entry_time.elapsed() > Duration::from_secs(10)  // 10s time threshold
}
```

Entries are **not sorted** before flush. Zigzag-encoded timestamp deltas handle out-of-order logs.

#### **4.7 Segment Building**

```
1. Snapshot buffer
   └── Swap with empty buffer (new writes go to fresh buffer)
   └── Caller owns snapshot, no lock held during S3 write

2. Compute min/max timestamp from entries

3. Encode entries:
   └── For each entry:
       - timestamp_delta = zigzag(entry.ts - min_ts)
       - Assign dictionary IDs for indexed values
       - Encode: delta | bitmap | dict_ids | payload_len | payload

4. Compress entries section (zstd)

5. Serialize dictionary

6. Build bloom filters (one per indexed field)

7. Build footer:
   └── log_count, min_ts, max_ts, offsets, checksum, magic

8. Concatenate: entries | dictionary | blooms | footer

9. Generate segment key:
   └── /namespaces/{ns_id}/segments/L0/{hour}/{ulid}.seg
   └── Hour derived from min_timestamp

10. PUT segment to S3

11. Update manifest:
    └── PUT log entry with "add" action
    └── PUT head.json with ETag validation
```

#### **4.8 Error Handling**

| Error | Handling |
|-------|----------|
| Invalid JSON in request | 400 Bad Request, reject entire request |
| Missing/invalid timestamp | Skip log, include in response errors |
| Namespace not found | 404 Not Found |
| S3 PUT failure | Retry with exponential backoff |
| Manifest update failure | Retry; segment becomes orphan if unrecoverable |

**Retry policy for S3:**

```rust
const MAX_RETRIES: u32 = 3;
const BASE_DELAY: Duration = Duration::from_millis(100);

// Exponential backoff: 100ms, 200ms, 400ms
```

#### **4.9 Backpressure**

| Condition | Response |
|-----------|----------|
| Total buffer memory > limit | 503 Service Unavailable |
| Pending flush queue full | 503 with Retry-After header |
| S3 errors exceed threshold | Circuit breaker opens, reject writes |

#### **4.10 Namespace Configuration**

Namespace metadata is stored in the **catalog**, an S3-backed data structure managed by the query engine itself. See Section 5.11 for catalog design.

```rust
struct NamespaceInfo {
    id: String,              // ns_01JQXYZ...
    name: String,            // Human-readable name (used in FROM clause)
    retention_days: u32,     // Default 7
    created_at: i64,
    updated_at: i64,
}
```

**Why not SQLite:**
- We're already building a SQL query engine
- Keeps "S3 as single source of truth" consistent
- One less component (no Litestream dependency)
- Catalog queries (`SHOW TABLES`, `DESCRIBE`) become first-class SQL

#### **4.11 Authentication**

Deferred to load balancer / reverse proxy. The indexer assumes requests are authenticated and authorized for the target namespace.

---

### **5. Detailed Design: Query Engine**

A custom SQL query engine built from scratch for learning. Processes queries through parsing, planning, optimization, and execution.

#### **5.1 Query Pipeline Overview**

```
SQL Query
    │
    ▼
┌──────────────┐
│    Parser    │  SQL text → Abstract Syntax Tree
└──────────────┘
    │
    ▼
┌──────────────┐
│   Logical    │  AST → Logical Plan (what to do)
│   Planner    │
└──────────────┘
    │
    ▼
┌──────────────┐
│   Logical    │  Apply rewrite rules (predicate pushdown, etc.)
│  Optimizer   │
└──────────────┘
    │
    ▼
┌──────────────┐
│   Physical   │  Logical Plan → Physical Plan (how to do it)
│   Planner    │  Segment selection, cache checks, bloom pruning
└──────────────┘
    │
    ▼
┌──────────────┐
│   Executor   │  Physical Plan → Results
│   (Volcano)  │  Fetch, decode, filter, aggregate, stream
└──────────────┘
```

#### **5.2 Execution Model**

**Volcano (Iterator):** Each operator implements `next()` returning one row at a time. Parent operators pull from children.

**Vectorized:** Operators process batches of rows (1000+) at once. SIMD-friendly, cache-friendly, faster for scans.

**Our choice:** Start with **Volcano**. Simpler to implement and debug. Migrate to Vectorized when benchmarks justify the complexity.

```rust
trait Operator {
    fn open(&mut self) -> Result<()>;
    fn next(&mut self) -> Result<Option<Row>>;
    fn close(&mut self) -> Result<()>;
}
```

#### **5.3 Optimization Strategy**

**Rule-based:** Apply pattern-matching rewrite rules. Deterministic, simple.

**Cost-based:** Estimate costs of different plans, pick cheapest. Requires statistics.

**Our choice:** **Rule-based**. Log queries are simple enough. Key rules:

| Rule | Effect |
|------|--------|
| Predicate pushdown | Move filters into Scan operator |
| Filter merge | Combine adjacent filters with AND |
| Projection pruning | Only read columns needed by query |
| Constant folding | Evaluate constant expressions at plan time |
| Limit pushdown | Propagate limit hints for early termination |

#### **5.4 Parser**

Custom SQL parser built from scratch.

**Supported syntax (v1):**

```sql
SELECT [columns | *]
FROM {namespace}
WHERE [predicates]
GROUP BY [columns]
ORDER BY [columns] [ASC|DESC]
LIMIT [n] OFFSET [m]
```

The `FROM` clause specifies the namespace to query. Each namespace appears as a table:

```sql
-- Query the api-gateway namespace
SELECT * FROM api_gateway WHERE level = 'error'

-- Query the payment-service namespace
SELECT timestamp, message FROM payment_service WHERE trace_id = 'abc123'
```

**Metadata queries:**

```sql
SHOW TABLES                    -- List all namespaces
DESCRIBE api_gateway           -- Show schema/indexed fields for namespace
```

**Predicates:**
- Comparison: `=`, `!=`, `<`, `>`, `<=`, `>=`
- Logical: `AND`, `OR`, `NOT`
- Patterns: `IN (...)`, `BETWEEN ... AND ...`
- Null checks: `IS NULL`, `IS NOT NULL`

**Output:** Abstract Syntax Tree (AST)

```rust
struct SelectStatement {
    columns: Vec<SelectColumn>,
    from: String,  // Namespace name (resolved to ID via catalog)
    where_clause: Option<Expr>,
    group_by: Vec<Expr>,
    order_by: Vec<OrderByExpr>,
    limit: Option<usize>,
    offset: Option<usize>,
}
```

#### **5.5 Logical Plan**

Tree of logical operators describing what to compute.

```rust
enum LogicalPlan {
    Scan {
        namespace: String,
        time_range: Option<(i64, i64)>,
    },
    Filter {
        input: Box<LogicalPlan>,
        predicate: Expr,
    },
    Project {
        input: Box<LogicalPlan>,
        expressions: Vec<(Expr, String)>,  // (expr, alias)
    },
    Aggregate {
        input: Box<LogicalPlan>,
        group_by: Vec<Expr>,
        aggregates: Vec<AggregateExpr>,
    },
    Sort {
        input: Box<LogicalPlan>,
        order_by: Vec<SortExpr>,
    },
    Limit {
        input: Box<LogicalPlan>,
        limit: usize,
        offset: usize,
    },
}
```

**Operators:**

| Operator | Purpose |
|----------|---------|
| Scan | Read logs from namespace, optionally with time range |
| Filter | Keep rows matching predicate |
| Project | Select/compute columns |
| Aggregate | Group rows, compute COUNT/SUM/MIN/MAX/AVG |
| Sort | Order rows by columns |
| Limit | Restrict output to N rows |

#### **5.6 Physical Planner**

Converts logical plan to executable physical plan. Key responsibilities:

**1. Manifest lookup:** Query manifest for segments matching namespace + time range.

**2. Bloom filter pruning:** Check bloom filters against predicates, remove segments that can't match.

**3. Block selection:** For L1/L2 segments, use block index to identify relevant blocks.

**4. Cache classification:** Partition segments into cached (memory/NVMe) vs cold (S3).

**5. Build physical operators:** Create concrete execution plan.

```rust
enum PhysicalPlan {
    SegmentScan {
        units: Vec<ScanUnit>,
        predicate: Option<PhysExpr>,
        projection: Vec<usize>,
    },
    FilterExec {
        input: Box<PhysicalPlan>,
        predicate: PhysExpr,
    },
    ProjectExec {
        input: Box<PhysicalPlan>,
        expressions: Vec<PhysExpr>,
    },
    HashAggregate {
        input: Box<PhysicalPlan>,
        group_by: Vec<usize>,
        aggregates: Vec<AggregateOp>,
    },
    SortExec {
        input: Box<PhysicalPlan>,
        order_by: Vec<(usize, SortOrder)>,
    },
    LimitExec {
        input: Box<PhysicalPlan>,
        limit: usize,
        offset: usize,
    },
}

enum ScanUnit {
    Segment { key: String, source: DataSource },
    Blocks { segment_key: String, blocks: Vec<usize>, source: DataSource },
}

enum DataSource {
    Memory,  // L1 cache (decoded blocks)
    NVMe,    // L2 cache (compressed segments)
    S3,      // Cold (fetch required)
}
```

#### **5.7 Physical Planning Decisions**

| Decision | Choice |
|----------|--------|
| Parallel S3 fetches | 4-8 concurrent (tunable) |
| Block prefetching | Prefetch next blocks while processing current |
| Scan ordering | Cache-first (process cached data immediately) |
| Predicate pushdown | Into SegmentScan when possible |

#### **5.8 Executor**

Volcano-style iterator execution.

```rust
impl SegmentScan {
    fn next(&mut self) -> Result<Option<Row>> {
        loop {
            // Get next row from current block
            if let Some(row) = self.current_block_iter.next() {
                // Apply pushed-down predicate
                if self.predicate.as_ref().map_or(true, |p| p.evaluate(&row).as_bool()) {
                    return Ok(Some(self.project(&row)));
                }
                continue;
            }

            // Current block exhausted, load next
            if !self.load_next_block()? {
                return Ok(None);  // All blocks done
            }
        }
    }

    fn load_next_block(&mut self) -> Result<bool> {
        let unit = self.units.next()?;
        match unit {
            None => Ok(false),
            Some(ScanUnit::Segment { key, source }) => {
                let data = self.fetch(key, source)?;
                self.current_block_iter = decode_segment(data)?;
                Ok(true)
            }
            Some(ScanUnit::Blocks { segment_key, blocks, source }) => {
                let data = self.fetch_blocks(segment_key, blocks, source)?;
                self.current_block_iter = decode_blocks(data)?;
                Ok(true)
            }
        }
    }
}
```

#### **5.9 Aggregate Execution**

Hash-based aggregation for GROUP BY:

```rust
impl HashAggregate {
    fn execute(&mut self) -> Result<()> {
        // Phase 1: Build hash table
        while let Some(row) = self.input.next()? {
            let key = self.extract_group_key(&row);
            let entry = self.table.entry(key).or_insert_with(|| AccumulatorSet::new());
            entry.accumulate(&row);
        }

        // Phase 2: Emit results (on subsequent next() calls)
        self.result_iter = self.table.drain();
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Row>> {
        self.result_iter.next().map(|(key, accum)| {
            Row::from_group(key, accum.finalize())
        })
    }
}
```

#### **5.10 Streaming Results**

Results stream as data becomes available:

```
Query starts
    │
    ├── Cached blocks processed immediately → results stream out
    │
    ├── S3 fetch starts in background
    │
    ├── More cached blocks processed → more results
    │
    ├── S3 fetch completes → decode → more results
    │
    └── All segments done → stream closes
```

Client sees first results quickly (from cache), with more arriving as cold data loads.

#### **5.11 Catalog**

The catalog stores namespace name ↔ ID mappings. Uses the same append-only log pattern as manifests for atomic updates.

**S3 structure:**

```
/metadata/
  ├── head.json                        # Current state pointer
  ├── snapshots/
  │   └── {timestamp}.json             # Compacted namespace list
  └── log/
      └── {ulid}.json                  # Individual operations
```

**head.json:**

```json
{
  "version": 1,
  "snapshot": "snapshots/2025-01-15T00:00:00Z.json",
  "log_tail": ["01JQXYZ1", "01JQXYZ2"]
}
```

**Snapshot:**

```json
{
  "created_at": "2025-01-15T00:00:00Z",
  "namespaces": [
    {"id": "ns_01JQXYZ123ABCDEFGHJKMNPQRS", "name": "api_gateway"},
    {"id": "ns_01JQABC456DEFGHJKLMNPQRST", "name": "payment_service"}
  ]
}
```

**Log entries:**

```json
{
  "id": "01JQXYZ1",
  "timestamp": 1705323700000,
  "action": "add",
  "namespace": {"id": "ns_01JQXYZ...", "name": "api_gateway"}
}
```

```json
{
  "id": "01JQXYZ2",
  "timestamp": 1705323800000,
  "action": "remove",
  "namespace_id": "ns_01JQXYZ..."
}
```

**Read flow:**

```
1. GET head.json (with ETag validation if cached)
2. GET snapshot (immutable, cache forever)
3. GET log entries from log_tail (immutable, cache forever)
4. Apply log entries to snapshot → current namespace list
```

**Write flow (e.g., CREATE TABLE):**

```
1. PUT log entry to /metadata/log/{ulid}.json (atomic, no conflict)
2. GET head.json with ETag
3. Append log entry ID to log_tail
4. PUT head.json with If-Match (retry on 412)
```

**Compaction:** When log grows large (>20 entries), compact into new snapshot.

**Query operations:**

| SQL | Implementation |
|-----|----------------|
| `SHOW TABLES` | Return `namespaces[*].name` |
| `DESCRIBE {name}` | Resolve ID → read manifest → return stats + indexed fields |
| `FROM {name}` | Resolve name → ID, query segments |
| `CREATE TABLE {name}` | Generate `ns_{ulid}`, append "add" to log |
| `DROP TABLE {name}` | Append "remove" to log, async segment cleanup |

**Caching:**

The catalog is small and rarely changes. Queriers cache it aggressively in L1 memory.

| Phase | Behavior |
|-------|----------|
| **Startup** | Fetch full catalog (snapshot + log), cache in memory |
| **Runtime** | Serve all queries from memory, no per-query validation |
| **Refresh** | Background refresh on change detection |

**Change detection (hybrid):**

1. **Tigris notifications (primary)**
   - Configure webhook with filter: `WHERE key REGEXP "^metadata/"`
   - Events: `OBJECT_CREATED_PUT` on head.json triggers refresh
   - Delivery: at-least-once, may be out-of-order (handled via version check)

2. **Periodic ETag poll (fallback)**
   - Every 60 seconds, `HEAD /metadata/head.json` with `If-None-Match`
   - 304 Not Modified → cache still valid
   - 200 OK → trigger catalog refresh

The notification provides near-instant refresh (<1s typical). The poll handles edge cases (missed notifications, webhook downtime).

**Deduplication:** Notifications may arrive multiple times. Use catalog version number to avoid redundant refreshes.

---

### **6. Caching**

Query nodes use a layered cache architecture to achieve <50ms p99 latency for hot queries.

#### **6.1 Cache Architecture**

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           Query Node                                    │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                      MEMORY CACHES                               │   │
│  │  ┌─────────────────┐ ┌─────────────────┐ ┌───────────────────┐  │   │
│  │  │  Block Cache    │ │  Bloom Cache    │ │ Manifest Cache    │  │   │
│  │  │  (L1)           │ │                 │ │                   │  │   │
│  │  │  Decoded blocks │ │  Bloom filters  │ │  head.json +      │  │   │
│  │  │  60-80% RAM     │ │  per-segment    │ │  snapshots + logs │  │   │
│  │  └─────────────────┘ └─────────────────┘ └───────────────────┘  │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                               │ miss                                    │
│                               ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    SEGMENT CACHE (L2) - NVMe                     │   │
│  │  Compressed segments, time-biased LRU, 100s GB+                  │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                               │ miss                                    │
│                               ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                         S3 (Tigris)                              │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

#### **6.2 Cache Summary**

| Cache | Contents | Location | Capacity | Eviction | Key |
|-------|----------|----------|----------|----------|-----|
| Block (L1) | Decoded blocks/segments | RAM | 60-80% of RAM | LRU | `(segment_key, block_idx)` |
| Bloom | Bloom filters per field | RAM | ~MBs | LRU | `(segment_key, field_id)` |
| Manifest | head.json, snapshots, log entries | RAM | Small per namespace | ETag-based | `namespace_id` |
| Segment (L2) | Compressed segments | NVMe | 100s GB - TBs | Time-biased LRU | `segment_key` |

#### **6.3 L0 vs L1/L2 Caching Differences**

L0 segments have no blocks. This affects caching granularity:

| Aspect | L0 Segment | L1/L2 Segment |
|--------|------------|---------------|
| Structure | Flat (no blocks) | 64KB blocks |
| Bloom filters | Per-field per-segment | Per-field per-segment (same) |
| L2 cache unit | Whole segment | Whole segment |
| L1 cache unit | Whole decoded segment | Individual decoded blocks |
| L1 cache key | `(segment_key, None)` | `(segment_key, Some(block_idx))` |
| Read pattern | Decode entire segment | Decode only needed blocks |

**ScanUnit reflects this difference:**

```rust
enum ScanUnit {
    // L0: whole segment is the unit
    L0Segment {
        key: String,
        source: DataSource,
    },
    // L1/L2: specific blocks within a segment
    Blocks {
        segment_key: String,
        block_indices: Vec<u32>,
        source: DataSource,
    },
}
```

#### **6.4 Block Cache (L1)**

In-memory cache of decoded data ready for scanning.

```rust
struct BlockCache {
    entries: HashMap<BlockKey, CachedBlock>,
    lru: LruList<BlockKey>,
    max_bytes: usize,
    current_bytes: usize,
}

struct BlockKey {
    segment_key: String,
    block_index: Option<u32>,  // None for L0, Some(n) for L1/L2
}

struct CachedBlock {
    entries: Vec<DecodedEntry>,
    size_bytes: usize,
}
```

**Caching logic by segment level:**

```rust
fn cache_decoded_data(segment: &Segment, needed_blocks: &[u32]) -> Result<()> {
    match segment.level {
        0 => {
            // L0: decode entire segment, cache as single entry
            let decoded = decode_l0_segment(&segment.data)?;
            block_cache.insert(
                BlockKey { segment_key: segment.key.clone(), block_index: None },
                CachedBlock { entries: decoded, size_bytes: ... }
            );
        }
        1 | 2 => {
            // L1/L2: decode only needed blocks, cache each separately
            for &block_idx in needed_blocks {
                let decoded = decode_block(&segment.data, block_idx)?;
                block_cache.insert(
                    BlockKey { segment_key: segment.key.clone(), block_index: Some(block_idx) },
                    CachedBlock { entries: decoded, size_bytes: ... }
                );
            }
        }
    }
    Ok(())
}
```

**Sizing:** Query nodes are dedicated to querying. Cache can consume 60-80% of available RAM.

**Eviction:** Pure LRU. When inserting would exceed capacity, evict least recently used entries.

#### **6.5 Bloom Cache**

Separate in-memory cache for bloom filters. Enables fast segment pruning without NVMe reads.

Bloom filters are per-field per-segment for both L0 and L1/L2 (no difference).

```rust
struct BloomCache {
    entries: HashMap<BloomKey, BloomFilter>,
    lru: LruList<BloomKey>,
    max_entries: usize,
}

struct BloomKey {
    segment_key: String,
    field_id: u8,  // Index into AUTO_INDEX_FIELDS
}
```

**Population:** When a segment footer is read (from L2 or S3), extract all bloom filters and cache them.

**Usage during physical planning:**

```rust
fn should_read_segment(segment: &SegmentInfo, predicate: &Expr) -> bool {
    for (field_id, value) in extract_equality_predicates(predicate) {
        let key = BloomKey { segment_key: segment.key.clone(), field_id };

        if let Some(bloom) = bloom_cache.get(&key) {
            if !bloom.might_contain(value) {
                return false;  // Definitely no matches, prune segment
            }
        }
        // Bloom not cached or says "maybe" - keep segment
    }
    true
}
```

#### **6.6 Manifest Cache**

Per-namespace cache of manifest data.

```rust
struct ManifestCache {
    entries: HashMap<String, CachedManifest>,
}

struct CachedManifest {
    head: HeadJson,
    head_etag: String,
    snapshot: Snapshot,
    log_entries: Vec<LogEntry>,
    last_validated: Instant,
}
```

**Freshness via ETag validation:**

```rust
async fn get_manifest(namespace_id: &str) -> Result<Manifest> {
    if let Some(cached) = manifest_cache.get(namespace_id) {
        // Conditional GET - only fetch if changed
        match s3.get_if_none_match(head_key, &cached.head_etag).await? {
            NotModified => return Ok(cached.to_manifest()),
            Modified(new_head, new_etag) => {
                let manifest = load_manifest_from_head(new_head).await?;
                manifest_cache.insert(namespace_id, manifest.clone(), new_etag);
                return Ok(manifest);
            }
        }
    }

    // Cold load
    let manifest = load_full_manifest(namespace_id).await?;
    manifest_cache.insert(namespace_id, manifest.clone());
    Ok(manifest)
}
```

#### **6.7 Segment Cache (L2)**

NVMe-based cache of compressed segments exactly as stored in S3.

```rust
struct SegmentCache {
    index: HashMap<String, SegmentCacheEntry>,
    base_path: PathBuf,  // e.g., /var/cache/sybil/segments/
    max_bytes: u64,
    current_bytes: u64,
}

struct SegmentCacheEntry {
    segment_key: String,
    local_path: PathBuf,
    size_bytes: u64,
    segment_min_ts: i64,  // For time-biased eviction
    last_accessed: Instant,
}
```

**File layout on NVMe:**

```
/var/cache/sybil/segments/
  └── {hash(segment_key)}.seg
```

**Time-biased LRU eviction:**

Recent segments (by data timestamp) are stickier than old segments. Keeps hot/recent logs in cache even when queries scan historical data.

```rust
fn eviction_score(entry: &SegmentCacheEntry) -> f64 {
    let access_age_hours = entry.last_accessed.elapsed().as_secs_f64() / 3600.0;
    let data_age_hours = (now_millis() - entry.segment_min_ts) as f64 / 3_600_000.0;

    // Higher score = keep longer
    let access_factor = 1.0 / (1.0 + access_age_hours);
    let recency_factor = 1.0 / (1.0 + data_age_hours / 24.0);

    access_factor * recency_factor
}

fn evict_until_space(&mut self, needed: u64) {
    while self.current_bytes + needed > self.max_bytes {
        let victim = self.index.values()
            .min_by(|a, b| eviction_score(a).partial_cmp(&eviction_score(b)).unwrap());

        if let Some(v) = victim {
            fs::remove_file(&v.local_path)?;
            self.current_bytes -= v.size_bytes;
            self.index.remove(&v.segment_key);
        }
    }
}
```

#### **6.8 Read Flow with Caching**

```
Query: SELECT * FROM api_gateway WHERE level = 'error' AND timestamp > now() - 1h

1. MANIFEST CACHE
   └── Get active segments for namespace + time range
   └── ETag validation ensures freshness

2. BLOOM CACHE (same for L0 and L1/L2)
   └── For each candidate segment:
       └── Check bloom filter for 'error' in level field
       └── Hit + definitely not → prune segment
       └── Hit + maybe or miss → keep segment

3. FOR EACH L0 SEGMENT:
   └── L1 cache hit (key: segment, None)?
       └── Yes → return decoded segment (microseconds)
       └── No → check L2, decode entire segment, cache in L1

4. FOR EACH L1/L2 SEGMENT:
   └── Identify needed blocks via block index
   └── For each block:
       └── L1 cache hit (key: segment, block_idx)?
           └── Yes → return decoded block (microseconds)
           └── No → check L2, decode block, cache in L1

5. L2 CACHE (same for all levels)
   └── Segment on NVMe?
       └── Yes → read from NVMe (1-5ms)
       └── No → fetch from S3 (50-100ms), write to L2

6. EXECUTE
   └── Filter, project, aggregate
   └── Stream results to client
```

#### **6.9 Cache Warming**

No proactive cache warming. Query nodes pull data on demand.

**Rationale:**
- Ingestion and query nodes are separate (stateless architecture)
- Fresh data naturally warms cache on first query
- Most queries target recent data, which gets queried frequently

#### **6.10 Cache Invalidation**

| Cache | Invalidation Strategy |
|-------|----------------------|
| Block (L1) | Eviction only (segments are immutable) |
| Bloom | Eviction only (segments are immutable) |
| Manifest | ETag validation on head.json |
| Segment (L2) | Eviction only (segments are immutable) |

Segments are immutable once written. No invalidation needed except for eviction pressure.

Manifest freshness ensured via S3 conditional GET with ETag.

---

### **7. Detailed Design: Steward**

The Steward is a background coordinator responsible for housekeeping tasks: compaction, secondary indexes, garbage collection, and retention enforcement.

#### **7.1 Architecture Overview**

```
┌─────────────────────────────────────────────────────────────┐
│                    Steward (single instance)                 │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ Scheduler                                               │ │
│  │ - HTTP endpoint (receives Tigris notifications)        │ │
│  │ - Heartbeat timer (triggers scheduled work)            │ │
│  │ - Work queue (prioritized by urgency)                  │ │
│  └──────────────────────────┬─────────────────────────────┘ │
│                             │                                │
│                             ▼                                │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ Worker Pool (Tokio tasks)                               │ │
│  │ - Bounded concurrency (e.g., 10 concurrent tasks)       │ │
│  │ - Stateless: read from S3, do work, write to S3         │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
              │                              │
              ▼                              ▼
┌──────────────────────────┐    ┌──────────────────────────┐
│      Tigris (S3)         │    │     Tigris (S3)          │
│  (read segments)         │    │  (write segments,        │
│                          │    │   update manifest)       │
└──────────────────────────┘    └──────────────────────────┘
```

**Design principles:**

| Principle | Implementation |
|-----------|----------------|
| Stateless | All state derived from S3/manifest on startup |
| Minimal resources | Coordinator is lightweight; heavy work in workers |
| No coordination service | Manifest ETag provides atomic updates, no locks |
| Recoverable | On restart, scan manifest to identify pending work |

#### **7.2 Triggering Mechanisms**

The Steward responds to two trigger sources:

**1. Tigris Object Notifications (reactive)**

Configured via [Tigris bucket notifications](https://www.tigrisdata.com/docs/buckets/object-notifications/):

```
Webhook: https://steward.sybil.example.com/notify
Filter: prefix=/namespaces/
Events: OBJECT_CREATED_PUT
```

When an L0 segment is written, Tigris sends a notification. The Steward checks if compaction thresholds are met.

**2. Heartbeat Timer (scheduled)**

Internal timer triggers periodic work:

| Task | Frequency |
|------|-----------|
| L1→L2 compaction | Daily (at day boundary) |
| Retention enforcement | Daily |
| GC (superseded segments) | Hourly |
| Orphan cleanup | Daily |

```rust
// Pseudocode
async fn heartbeat_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;

        let now = Utc::now();
        if now.hour() == 0 && now.minute() == 0 {
            queue.push(Task::CompactL1ToL2 { date: now.date() - 1 });
            queue.push(Task::EnforceRetention);
            queue.push(Task::CleanupOrphans);
        }
        queue.push(Task::GarbageCollect);
    }
}
```

#### **7.3 Work Types**

| Task | Trigger | Description |
|------|---------|-------------|
| **L0→L1 Compaction** | L0 notification | Merge L0 segments when count > threshold (e.g., 10 per hour) |
| **L1→L2 Compaction** | Heartbeat (daily) | Merge hourly L1 segments into daily L2 segment |
| **Secondary Indexes** | After L1/L2 compaction | Build full-text inverted index on `message` field |
| **GC (superseded)** | Heartbeat (hourly) | Delete superseded segments after TTL (e.g., 60s) |
| **Retention** | Heartbeat (daily) | Mark segments older than `retention_days` as superseded |
| **Orphan Cleanup** | Heartbeat (daily) | Compare S3 listing to manifest, delete orphans |

#### **7.4 L0→L1 Compaction**

**Trigger:** Tigris notification for new L0 segment.

**Check condition:**

```rust
async fn maybe_compact_l0(namespace: &str, hour: &str) -> Result<()> {
    let manifest = load_manifest(namespace).await?;
    let l0_segments: Vec<_> = manifest.segments
        .iter()
        .filter(|s| s.level == 0 && s.hour == hour && s.state == Active)
        .collect();

    if l0_segments.len() >= L0_COMPACTION_THRESHOLD {
        queue.push(Task::CompactL0 { namespace, hour, segments: l0_segments });
    }
    Ok(())
}
```

**Compaction flow:**

```
1. Read all L0 segments for the hour from S3
2. Merge entries, sort by timestamp
3. Build L1 segment (64KB blocks, per-block dictionary)
4. Build bloom filters
5. PUT L1 segment to S3: /namespaces/{ns}/segments/L1/{date}/{hour}.seg
6. Update manifest atomically:
   - Add L1 segment (state: active)
   - Mark L0 segments as superseded
   - PUT with If-Match ETag (retry on 412)
```

**Conflict resolution:** If two workers race, only one succeeds at manifest update. The loser's L1 becomes an orphan, cleaned up later.

#### **7.5 L1→L2 Compaction**

**Trigger:** Heartbeat at day boundary.

**Flow:**

```
1. For each namespace:
   a. Load manifest
   b. Find all L1 segments for yesterday
   c. Merge into single L2 segment (~256MB, 64KB blocks)
   d. PUT L2 to S3: /namespaces/{ns}/segments/L2/{date}.seg
   e. Update manifest: add L2, supersede L1s
```

#### **7.6 Secondary Indexes (Full-Text)**

**Purpose:** Enable fast `message LIKE '%error%'` queries without full segment scans.

**Trigger:** After L1 or L2 compaction completes.

**Index structure:**

```
/namespaces/{ns}/indexes/fulltext/{date}.idx

Index format:
┌────────────────────────────────────────┐
│ Term Dictionary                         │
│   term → (doc_count, posting_list_off)  │
├────────────────────────────────────────┤
│ Posting Lists                           │
│   term1: [seg_key:entry_id, ...]        │
│   term2: [seg_key:entry_id, ...]        │
├────────────────────────────────────────┤
│ Footer                                  │
│   term_count, checksum, magic           │
└────────────────────────────────────────┘
```

**Build process:**

```
1. Read L1/L2 segment
2. For each log entry, tokenize message field
3. Build inverted index: term → list of (segment_key, entry_offset)
4. Compress and write to S3
5. Update manifest with index reference
```

#### **7.7 Garbage Collection**

**Superseded segment cleanup:**

Segments marked `superseded` during compaction are deleted after a TTL (60 seconds) to allow in-flight queries to complete.

```rust
async fn garbage_collect(namespace: &str) -> Result<()> {
    let manifest = load_manifest(namespace).await?;
    let now = Utc::now();

    let expired: Vec<_> = manifest.segments
        .iter()
        .filter(|s| s.state == Superseded && now - s.superseded_at > TTL)
        .collect();

    for segment in &expired {
        s3.delete(&segment.key).await?;
    }

    // Remove from manifest
    update_manifest(namespace, |m| {
        m.segments.retain(|s| !expired.contains(&s));
    }).await?;

    Ok(())
}
```

**Orphan cleanup:**

Orphans are segments in S3 but not in manifest (e.g., from crashed compaction).

```rust
async fn cleanup_orphans(namespace: &str) -> Result<()> {
    let manifest = load_manifest(namespace).await?;
    let manifest_keys: HashSet<_> = manifest.segments.iter().map(|s| &s.key).collect();

    let s3_keys = s3.list_prefix(&format!("/namespaces/{}/segments/", namespace)).await?;

    for key in s3_keys {
        if !manifest_keys.contains(&key) {
            let metadata = s3.head(&key).await?;
            if metadata.age() > Duration::from_secs(300) {  // 5 min grace period
                s3.delete(&key).await?;
            }
        }
    }
    Ok(())
}
```

#### **7.8 Retention Enforcement**

```rust
async fn enforce_retention(namespace: &str) -> Result<()> {
    let config = load_namespace_config(namespace).await?;
    let cutoff = Utc::now() - Duration::days(config.retention_days as i64);

    update_manifest(namespace, |m| {
        for segment in &mut m.segments {
            if segment.max_timestamp < cutoff && segment.state == Active {
                segment.state = Superseded;
                segment.superseded_at = Utc::now();
            }
        }
    }).await?;

    Ok(())
}
```

#### **7.9 Worker Pool**

Workers are Tokio tasks with bounded concurrency:

```rust
struct Steward {
    work_queue: mpsc::Receiver<Task>,
    semaphore: Arc<Semaphore>,  // Limits concurrent tasks
}

impl Steward {
    async fn run(&mut self) {
        while let Some(task) = self.work_queue.recv().await {
            let permit = self.semaphore.clone().acquire_owned().await.unwrap();

            tokio::spawn(async move {
                let result = execute_task(task).await;
                if let Err(e) = result {
                    tracing::error!(?task, ?e, "task failed");
                    // Could re-queue with backoff
                }
                drop(permit);
            });
        }
    }
}
```

**Concurrency limits:**

| Resource | Limit | Rationale |
|----------|-------|-----------|
| Concurrent tasks | 10 | Bound memory/CPU usage |
| S3 requests per task | 4-8 parallel | Match S3 parallelism sweet spot |

#### **7.10 Failure Recovery**

On startup, the Steward scans all namespaces to identify pending work:

```rust
async fn recover_pending_work() -> Result<Vec<Task>> {
    let mut tasks = Vec::new();

    for namespace in list_namespaces().await? {
        let manifest = load_manifest(&namespace).await?;

        // Check for L0 compaction needs
        let l0_by_hour = group_by_hour(&manifest.segments, Level::L0);
        for (hour, segments) in l0_by_hour {
            if segments.len() >= L0_COMPACTION_THRESHOLD {
                tasks.push(Task::CompactL0 { namespace, hour, segments });
            }
        }

        // Check for stale superseded segments
        let stale = manifest.segments.iter()
            .filter(|s| s.state == Superseded && s.age() > TTL)
            .collect();
        if !stale.is_empty() {
            tasks.push(Task::GarbageCollect { namespace });
        }
    }

    Ok(tasks)
}
```

#### **7.11 Scaling Path**

| Phase | Steward Count | Worker Model | When to Upgrade |
|-------|---------------|--------------|-----------------|
| **MVP** | 1 | Tokio tasks | Start here |
| **Scale** | 1 | Dormant Fly Machines | When tasks need isolation or more resources |
| **Large** | N | Fly Machines + consistent hashing | When single Steward becomes bottleneck |

**Spawning Fly Machines (future):**

```rust
async fn spawn_worker(task: Task) -> Result<()> {
    let machine_id = find_dormant_worker().await?;

    // Start machine with task payload
    fly_api::start_machine(machine_id, json!({
        "task": task,
        "callback": "https://steward.sybil.example.com/complete"
    })).await?;

    Ok(())
}
```

Fly Machine start time is ~300ms for dormant machines, making this viable for bursty workloads.

---

### **8. Observability**

The system will be instrumented with observability as a primary concern, guided by our SLAs.

#### **8.1 Instrumentation**

- **Tracing:** Every query and ingestion request is assigned a `trace_id` propagated through all components. Trace spans map directly to the latency stages in Section 9, enabling validation of napkin math against real-world performance.
- **Logging:** All components output structured (JSON) logs with rich context: `trace_id`, `namespace_id`, operation type, and latencies.
- **Tooling:** OpenTelemetry for vendor-neutral instrumentation. Exports to Jaeger (tracing), Prometheus (metrics), or hosted providers (Grafana Cloud, Datadog).

**Required trace spans (derived from Section 9 latency budgets):**

| Component | Span | Validates |
|-----------|------|-----------|
| Querier | `query.parse` | SQL parsing + planning (~1ms) |
| Querier | `query.manifest_lookup` | Manifest cache/fetch (~0.1ms cached, ~10ms cold) |
| Querier | `query.bloom_check` | Bloom filter pruning (~0.1ms) |
| Querier | `query.cache_lookup` | L1/L2 cache hit/miss |
| Querier | `query.s3_fetch` | Segment fetch (~20ms) |
| Querier | `query.decompress` | Zstd decode (~2ms) |
| Querier | `query.scan` | Entry filtering (~2ms per 1000 entries) |
| Querier | `query.aggregate` | GROUP BY / ORDER BY (~1ms) |
| Indexer | `ingest.validate` | Timestamp + field extraction (~1ms) |
| Indexer | `ingest.buffer` | Memtable insert (~0.1ms) |
| Indexer | `ingest.build_segment` | Dictionary + bloom + compress (~50ms) |
| Indexer | `ingest.s3_put` | Write L0 segment (~30ms) |
| Indexer | `ingest.manifest_update` | Atomic manifest update (~20ms) |
| Steward | `compact.read_segments` | Fetch segments to merge |
| Steward | `compact.merge` | Merge + sort entries |
| Steward | `compact.write_segment` | Write compacted segment |
| Steward | `compact.manifest_update` | Atomic manifest swap |

#### **8.2 Metrics**

**System-wide:**

| Metric | Component | Purpose |
|--------|-----------|---------|
| Query latency (p50, p99) | Querier | Track against <50ms hot, <100ms cold targets |
| Ingestion-to-queryable latency | Indexer | Track against ≤10s target |
| Cache hit rate | Querier | L1/L2 cache effectiveness |
| S3 request latency | All | Detect storage slowdowns |
| Error rates | All | By component and error type |

**Per-namespace:**

| Metric | Purpose |
|--------|---------|
| Query latency | Identify slow namespaces |
| Segment count (by level) | Storage health, compaction effectiveness |
| Storage size | Capacity planning, cost attribution |
| Ingestion rate | Traffic patterns |

**Steward tasks:**

| Metric | Purpose |
|--------|---------|
| Task queue depth | Backlog visibility |
| Compaction duration | Performance tracking |
| L0 segment backlog | Compaction lag per namespace |
| Task success/failure rate | Reliability |

#### **8.3 Health Checks**

Each component exposes endpoints for Fly liveness/readiness probes:

| Endpoint | Purpose |
|----------|---------|
| `GET /health/live` | Process is alive (always 200 if running) |
| `GET /health/ready` | Ready to serve traffic (S3 reachable, caches warm) |

#### **8.4 Alerting Thresholds**

| Condition | Severity | Action |
|-----------|----------|--------|
| Query p99 > 100ms (hot) | Warning | Investigate cache, S3 latency |
| Query p99 > 500ms | Critical | Page on-call |
| Ingestion-to-queryable > 30s | Warning | Check Indexer flush, S3 writes |
| Error rate > 1% | Warning | Investigate logs |
| Error rate > 5% | Critical | Page on-call |
| L0 backlog > 50 segments | Warning | Steward may be behind |
| Steward task failures > 3 consecutive | Critical | Compaction broken |

---

### **9. Scalability & Performance**

Napkin math to validate the design meets our SLAs. Will be refined with benchmarks post-MVP.

#### **9.1 Query Latency (Hot Path: <50ms p99)**

Target: Cached segments on NVMe, indexed query.

| Stage | Action | P99 Latency | Cumulative |
|-------|--------|-------------|------------|
| 1. Parse | SQL parsing + planning | ~1ms | 1ms |
| 2. Manifest | Cached manifest lookup | ~0.1ms | 1.1ms |
| 3. Bloom check | In-memory bloom filter | ~0.1ms | 1.2ms |
| 4. L1 cache | Decoded block lookup | ~0.5ms | 1.7ms |
| 5. Scan | Filter ~1000 entries | ~2ms | 3.7ms |
| 6. Aggregate | GROUP BY / ORDER BY | ~1ms | 4.7ms |
| 7. Serialize | JSON response | ~0.5ms | 5.2ms |
| **Total** | | | **~5ms** |

Comfortable margin under 50ms target. Even with 10x more data to scan, we stay under budget.

#### **9.2 Query Latency (Cold Path: <100ms p99)**

Target: Segment not cached, requires S3 fetch.

| Stage | Action | P99 Latency | Cumulative |
|-------|--------|-------------|------------|
| 1. Parse + Plan | Same as hot | ~1ms | 1ms |
| 2. Manifest | ETag validation (S3 HEAD) | ~10ms | 11ms |
| 3. Bloom check | Fetch footer if not cached | ~15ms | 26ms |
| 4. S3 fetch | GET segment (≤64KB L0) | ~20ms | 46ms |
| 5. Decompress | Zstd decode | ~2ms | 48ms |
| 6. Scan + filter | Same as hot | ~2ms | 50ms |
| 7. Serialize | JSON response | ~0.5ms | 50.5ms |
| **Total** | | | **~50ms** |

For L1/L2 segments with block index, we fetch only relevant blocks (parallel range requests), keeping latency similar despite larger segment size.

Under 100ms target with room for multiple segment fetches.

#### **9.3 Ingestion-to-Queryable (≤10 seconds)**

Target: Time from log received to queryable via SQL.

| Stage | Action | P99 Latency | Cumulative |
|-------|--------|-------------|------------|
| 1. HTTP receive | Parse request body | ~5ms | 5ms |
| 2. Validate | Timestamp + field extraction | ~1ms | 6ms |
| 3. Buffer | Add to memtable | ~0.1ms | 6.1ms |
| 4. Wait | Time-based flush (max) | 10,000ms | 10,006ms |
| 5. Build segment | Dictionary + bloom + compress | ~50ms | 10,056ms |
| 6. S3 PUT | Write L0 segment | ~30ms | 10,086ms |
| 7. Manifest update | Atomic update with ETag | ~20ms | 10,106ms |
| **Total** | | | **~10s** |

The 10-second flush window dominates. Actual P50 is much lower (size-based flush triggers earlier for active namespaces). Worst case meets the ≤10s target.

#### **9.4 Throughput (100s of GB/day)**

**Ingestion capacity (single Indexer):**

| Parameter | Value |
|-----------|-------|
| Average log size | ~500 bytes |
| L0 segment size | ≤64KB (inlined) |
| Logs per segment | ~200 (after compression) |
| S3 PUT latency | ~30ms |
| Segment builds/sec | ~30 |
| **Logs/sec (single Indexer)** | **~6,000** |
| **GB/day (single Indexer)** | **~250 GB** |

A single Indexer handles 250 GB/day. Two Indexers cover 500 GB/day with headroom.

**Query capacity (single Querier):**

| Parameter | Value |
|-----------|-------|
| Hot query latency | ~5ms |
| Cold query latency | ~50ms |
| Mix (80% hot, 20% cold) | ~14ms avg |
| **Queries/sec (single Querier)** | **~70** |

For higher QPS, add Queriers. Consistent hashing by namespace ensures cache locality.

#### **9.5 Storage Estimates**

**7-day retention at 100 GB/day:**

| Level | Data | Calculation |
|-------|------|-------------|
| L0 | ~10 GB | 1 day of recent segments before compaction |
| L1 | ~50 GB | ~5 days of hourly compacted (some already in L2) |
| L2 | ~200 GB | 7 days of daily archives |
| **Total** | **~260 GB** | Per 100 GB/day ingestion rate |

Compression ratio (~5x for JSON logs) means 100 GB/day raw ≈ 20 GB/day stored, so actual storage closer to **~50 GB** for 7 days.

#### **9.6 Scaling Levers**

| Bottleneck | Scaling Action |
|------------|----------------|
| Ingestion throughput | Add Indexer instances |
| Query throughput | Add Querier instances |
| Query latency (cold) | Larger NVMe cache, more Queriers |
| Compaction backlog | Steward spawns Fly Machine workers |
| Storage cost | Reduce retention, increase compression |

#### **9.7 Summary**

| SLA | Target | Napkin Estimate | Margin |
|-----|--------|-----------------|--------|
| Query latency (hot) | <50ms p99 | ~5ms | 10x |
| Query latency (cold) | <100ms p99 | ~50ms | 2x |
| Ingestion-to-queryable | ≤10s | ~10s | At limit |
| Throughput | 100s GB/day | 250 GB/day per Indexer | 2x+ |

The design has comfortable margins on query latency and throughput. Ingestion-to-queryable is tight at the 10s flush window—could reduce to 5s if needed, trading segment size efficiency for lower latency.

---

## References

- [Tigris Small Object Benchmarks](https://www.tigrisdata.com/blog/benchmark-small-objects/) — sub-10ms read latency via inlining, key coalescing, LSM-backed caching
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture) — 8ms p50 warm queries with NVMe cache, WAL design, namespace prefixes
- [Hedwig Design Document](https://github.com/rugwirobaker/hedwig/blob/main/docs/design.md) — S3-backed event delivery system with WAL, batching, LSM dedup index
