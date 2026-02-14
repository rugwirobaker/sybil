# Sybil

Sybil is an application logs storage and querying system built on S3-compatible storage with an SQL query interface.

## Architecture

The system consists of three stateless, independently scalable components that use S3-compatible object storage (Tigris) as the single source of truth.

1. **Indexer** — Accepts logs via HTTP, extracts indexed fields, builds bloom filters, and writes L0 segments to S3. Data is immediately queryable.
2. **Steward** — Background housekeeping: compacts segments (L0→L1→L2), builds secondary indexes (full-text search on `message` field), garbage collection.
3. **Querier** — Executes SQL queries. Uses primary indexes and bloom filters for fast lookups, falls back to segment scan when needed.

Each lives in a dedicated module but they share a lib module with components like the the `ObjectStore`.
And sybil is released as a single binary that can run in different modes(indexer, steward, querier) to simplify ops.

## Configuration

Sybil supports multiple configuration methods with the following hierarchy (highest to lowest priority):

1. **Command-line flags** (highest priority)
2. **Environment variables**
3. **Configuration file** (TOML format)
4. **Default values** (lowest priority)

### Configuration File

Create a `config.toml` file:

```toml
debug = true

# Storage configuration (choose one backend)
[storage]
backend = "memory"  # "memory" or "s3"

# S3-compatible storage configuration (when backend = "s3")
[storage.s3]
endpoint = "https://fly.storage.tigris.dev"
access_key_id = "your_key"
secret_access_key = "your_secret"
bucket = "sybil"
region = "auto"

[indexer]
bind = "0.0.0.0:9080"

[indexer.buffer]
flush_interval_secs = 10
segment_size_bytes = 49152  # 48KB

[querier]
bind = "0.0.0.0:8080"
```

## Running

Start the indexer:

```sh
./target/debug/sybil --config config.toml --indexer
```

## Indexer API

The indexer listens on the configured bind address (default `0.0.0.0:9080`).

### Namespaces

Namespaces isolate log streams. A namespace must exist before ingesting logs into it.

**Create a namespace**

```sh
curl -X POST http://localhost:9080/v1/namespaces \
  -H 'Content-Type: application/json' \
  -d '{"name": "test-app", "retention_days": 30}'
```

`retention_days` defaults to 7 if omitted. Names must be alphanumeric, hyphens, or underscores (1-128 chars).

**List namespaces**

```sh
curl http://localhost:9080/v1/namespaces
```

**Get a namespace**

```sh
curl http://localhost:9080/v1/namespaces/test-app
```

**Delete a namespace**

```sh
curl -X DELETE http://localhost:9080/v1/namespaces/test-app
```

Returns `204 No Content` on success.

### Log Ingestion

**Ingest logs**

```sh
curl -X POST http://localhost:9080/v1/logs/test-app \
  -H 'Content-Type: application/json' \
  -d '{
    "logs": [
      {"payload": {"level": "INFO", "message": "application started", "service": "api-gateway"}},
      {"timestamp": 1739501940000, "payload": {"level": "ERROR", "message": "connection refused", "service": "user-service"}}
    ]
  }'
```

Each log entry has:
- `payload` (required) — arbitrary JSON object
- `timestamp` (optional) — epoch milliseconds; defaults to server time if omitted

Returns `202 Accepted` with `{"accepted": <count>}`.

### Health

```sh
curl http://localhost:9080/health   # liveness
curl http://localhost:9080/ready    # readiness
```
