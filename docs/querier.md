# Querier Implementation Plan

This document describes the implementation plan for the Sybil querier — the SQL
query engine that reads segments from S3 and returns results.

Reference: `docs/design.md` Section 5 (Query Engine).

## What Already Exists

| Component | Location | Key Types |
|-----------|----------|-----------|
| Storage abstraction | `crates/lib/src/storage/engine.rs` | `ObjectStore`, `ByteRange`, `ObjectKey` |
| L0 segment format | `crates/lib/src/segment/` | `Builder`, `Reader`, `ProcessedEntry`, `Block` |
| Dictionary encoding | `crates/lib/src/segment/dictionary.rs` | `Dictionary` (id↔string) |
| Bloom filters | `crates/lib/src/segment/bloom.rs` | `BloomFilter` |
| Entry types | `crates/lib/src/segment/entry.rs` | `PresenceBitmap`, `AUTO_INDEX_FIELDS`, `field_index()` |
| Manifest | `crates/lib/src/manifest/mod.rs` | `SegmentInfo`, `State::segments_in_range()` |
| Catalog | `crates/lib/src/catalog/mod.rs` | `Namespace`, `State::get_by_name()` |
| Cache stack | `crates/lib/src/cache/` | `CacheManager`, `BlockCache`, `BloomCache`, `ManifestCache`, `CatalogCache` |
| Indexer service | `crates/indexer/` | Working HTTP ingest + flush to S3 |
| Querier crate | `crates/querier/src/lib.rs` | Stub (`todo!()`) |

## Query Pipeline

From design doc Section 5.1:

```
SQL text → Parser → Logical Planner → Optimizer → Physical Planner → Executor → Results
```

## Module Layout

All query engine code lives in `crates/lib/src/query/`:

```
crates/lib/src/query/
  ├── mod.rs          # Public API, re-exports
  ├── ast.rs          # AST types (shared by parser + planner)
  ├── parser.rs       # Lexer + recursive descent parser
  ├── planner.rs      # AST → Logical plan
  ├── optimizer.rs    # Rule-based logical plan rewriting
  ├── physical.rs     # Logical plan → Physical plan + operators
  └── executor.rs     # Volcano-style execution engine
```

The HTTP wiring lives in `crates/querier/` (like the indexer pattern).

## Chunk 1: SQL Parser

**Goal:** SQL string → AST. No I/O, no storage dependencies.

**Approach:** Hand-written recursive descent parser. Two phases: lexing (text →
tokens) and parsing (tokens → AST).

### Lexer

Tokenizes SQL text into a stream of tokens:

```rust
enum Token {
    // Keywords
    Select, From, Where, And, Or, Not, In, Between, Is, Null,
    Like, As, Order, By, Group, Limit, Offset, Asc, Desc,
    Show, Tables, Describe, Explain, Analyze, Buffers, True, False,

    // Literals
    Integer(i64),
    Float(f64),
    StringLiteral(String),

    // Identifiers
    Ident(String),

    // Operators
    Eq, Neq, Lt, Gt, LtEq, GtEq,
    Plus, Minus, Star, Slash,

    // Punctuation
    LParen, RParen, Comma, Semicolon, Dot,

    // End
    Eof,
}
```

### AST Types

```rust
/// Top-level statement.
enum Statement {
    Select(SelectStatement),
    Explain {
        statement: Box<Statement>,     // Must be Select
        analyze: bool,                 // EXPLAIN vs EXPLAIN ANALYZE
        buffers: bool,                 // Include buffer/cache stats
    },
    ShowTables,
    Describe(String),
}

struct SelectStatement {
    columns: Vec<SelectColumn>,
    from: String,                      // Namespace name
    where_clause: Option<Expr>,
    group_by: Vec<Expr>,
    order_by: Vec<OrderByExpr>,
    limit: Option<usize>,
    offset: Option<usize>,
}

enum SelectColumn {
    Wildcard,                          // *
    Expr { expr: Expr, alias: Option<String> },
}

struct OrderByExpr {
    expr: Expr,
    ascending: bool,
}

/// Expression tree.
enum Expr {
    // Literals
    Integer(i64),
    Float(f64),
    StringLiteral(String),
    Boolean(bool),

    // References
    Column(String),

    // Comparison
    BinaryOp { left: Box<Expr>, op: BinaryOp, right: Box<Expr> },
    UnaryOp { op: UnaryOp, expr: Box<Expr> },

    // Special forms
    IsNull(Box<Expr>),
    IsNotNull(Box<Expr>),
    InList { expr: Box<Expr>, list: Vec<Expr>, negated: bool },
    Between { expr: Box<Expr>, low: Box<Expr>, high: Box<Expr>, negated: bool },
    Like { expr: Box<Expr>, pattern: Box<Expr>, negated: bool },

    // Functions / aggregates
    Function { name: String, args: Vec<Expr> },
}

enum BinaryOp {
    Eq, Neq, Lt, Gt, LtEq, GtEq,
    And, Or,
    Plus, Minus, Multiply, Divide,
}

enum UnaryOp {
    Not,
    Minus,
}
```

### Supported Grammar

```
statement       = explain_stmt | select_stmt | show_tables | describe ;
explain_stmt    = EXPLAIN [ANALYZE] [BUFFERS] select_stmt ;
show_tables     = SHOW TABLES ;
describe        = DESCRIBE ident ;
select_stmt     = SELECT columns FROM ident
                  [WHERE expr]
                  [GROUP BY expr_list]
                  [ORDER BY order_list]
                  [LIMIT integer [OFFSET integer]] ;
columns         = "*" | column_list ;
column_list     = column ("," column)* ;
column          = expr [AS ident] ;
expr            = or_expr ;
or_expr         = and_expr (OR and_expr)* ;
and_expr        = not_expr (AND not_expr)* ;
not_expr        = NOT not_expr | comparison ;
comparison      = addition ((= | != | < | > | <= | >=) addition
                  | IS [NOT] NULL
                  | [NOT] IN "(" expr_list ")"
                  | [NOT] BETWEEN addition AND addition
                  | [NOT] LIKE addition)? ;
addition        = multiplication ((+ | -) multiplication)* ;
multiplication  = unary ((* | /) unary)* ;
unary           = (- | NOT) unary | primary ;
primary         = integer | float | string | TRUE | FALSE | NULL
                | ident "(" [expr_list] ")"   -- function call
                | ident                        -- column reference
                | "(" expr ")" ;
```

### Test Cases

- Basic: `SELECT * FROM api_gateway`
- Filtered: `SELECT * FROM api_gateway WHERE level = 'error'`
- Time range: `WHERE timestamp > 1705323600000 AND timestamp < 1705327200000`
- Complex: `WHERE level = 'error' AND service IN ('api', 'web')`
- Aggregates: `SELECT level, COUNT(*) FROM api_gateway GROUP BY level`
- Order/limit: `ORDER BY timestamp DESC LIMIT 100`
- Metadata: `SHOW TABLES`, `DESCRIBE api_gateway`
- Explain: `EXPLAIN SELECT * FROM api_gateway WHERE level = 'error'`
- Explain analyze: `EXPLAIN ANALYZE SELECT * FROM api_gateway WHERE level = 'error'`
- Explain buffers: `EXPLAIN ANALYZE BUFFERS SELECT ...`
- Error cases: missing FROM, unclosed parens, invalid tokens, EXPLAIN without SELECT

## Chunk 2: Logical Planner

**Goal:** AST → logical plan tree. Resolves namespace names to IDs (needs
catalog), extracts time range predicates, classifies predicates as indexed vs.
unindexed.

### Logical Plan Types

```rust
enum LogicalPlan {
    Scan {
        namespace: String,             // Namespace ID (resolved from name)
        time_range: Option<(i64, i64)>,
    },
    Filter {
        input: Box<LogicalPlan>,
        predicate: Expr,
    },
    Project {
        input: Box<LogicalPlan>,
        expressions: Vec<(Expr, String)>,  // (expr, output_name)
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

### Planning Rules

1. `FROM namespace` → `Scan { namespace }` (resolve name → ID via catalog)
2. `WHERE` → `Filter` on top of Scan; extract `timestamp` comparisons into
   `Scan.time_range`
3. `SELECT columns` → `Project` on top of Filter
4. `SELECT aggregates ... GROUP BY` → `Aggregate` on top of Filter
5. `ORDER BY` → `Sort` on top of Project/Aggregate
6. `LIMIT/OFFSET` → `Limit` on top

### Test Cases

- `SELECT * FROM ns` → `Scan`
- `SELECT * FROM ns WHERE level = 'error'` → `Filter(Scan)`
- `SELECT * FROM ns WHERE ts > X AND ts < Y AND level = 'error'`
  → `Filter(predicate=level='error', Scan(time_range=(X,Y)))`
- `SELECT level, COUNT(*) FROM ns GROUP BY level`
  → `Aggregate(group_by=[level], aggs=[count], Scan)`

## Chunk 3: Logical Optimizer

**Goal:** Rewrite the logical plan for better execution.

### Rewrite Rules

| Rule | What it does |
|------|-------------|
| Predicate pushdown | Move Filter predicates closer to Scan |
| Filter merge | Combine adjacent `Filter(Filter(...))` into `Filter(AND(...))` |
| Projection pruning | Track which columns are actually needed downstream, avoid reading unused fields |
| Constant folding | Evaluate constant sub-expressions at plan time (e.g., `1 + 1` → `2`) |
| Limit pushdown | Add limit hints to Scan for early termination |

### Implementation

Each rule is a function `fn rewrite(plan: LogicalPlan) -> LogicalPlan` applied
in sequence. Top-down or bottom-up tree walk depending on the rule.

### Test Cases

For each rule: construct a plan, apply the rule, assert the rewritten shape.

- Predicate pushdown: `Project(Filter(Scan))` → `Project(Scan[with pushed filter])`
- Filter merge: `Filter(p1, Filter(p2, Scan))` → `Filter(p1 AND p2, Scan)`
- Constant folding: `WHERE 1 = 1 AND level = 'error'` → `WHERE level = 'error'`

## Chunk 4: Physical Planner + Executor

**Goal:** Convert logical plan to executable physical operators that read real
segments.

### Physical Plan Types

```rust
enum PhysicalPlan {
    SegmentScan {
        segments: Vec<ScanUnit>,
        predicate: Option<PhysExpr>,
        projection: Vec<String>,       // Column names to output
    },
    FilterExec {
        input: Box<PhysicalPlan>,
        predicate: PhysExpr,
    },
    ProjectExec {
        input: Box<PhysicalPlan>,
        expressions: Vec<(PhysExpr, String)>,
    },
    HashAggregate {
        input: Box<PhysicalPlan>,
        group_by: Vec<String>,
        aggregates: Vec<AggregateOp>,
    },
    SortExec {
        input: Box<PhysicalPlan>,
        order_by: Vec<(String, bool)>,  // (column, ascending)
    },
    LimitExec {
        input: Box<PhysicalPlan>,
        limit: usize,
        offset: usize,
    },
}

enum ScanUnit {
    L0Segment { key: String },
    // L1/L2 blocks deferred to post-MVP
}
```

### Operator Trait (Volcano)

```rust
trait Operator {
    fn open(&mut self) -> Result<()>;
    fn next(&mut self) -> Result<Option<Row>>;
    fn close(&mut self) -> Result<()>;
    fn stats(&self) -> OperatorStats;
}
```

A `Row` is a map of column names to values. For the MVP, we can use
`serde_json::Value` or a lightweight enum:

```rust
enum Value {
    Null,
    Integer(i64),
    Float(f64),
    String(String),
    Boolean(bool),
}
```

### Execution Statistics (EXPLAIN ANALYZE + BUFFERS)

Every operator collects stats during execution. These are used to produce
EXPLAIN ANALYZE output and will be the baseline for comparing executor
strategies (volcano → vectorized).

```rust
/// Per-operator execution stats, collected during execution.
struct OperatorStats {
    /// Operator name (e.g., "SegmentScan", "FilterExec", "HashAggregate")
    operator: String,
    /// Rows produced by this operator
    rows_produced: u64,
    /// Rows consumed from child (input)
    rows_consumed: u64,
    /// Wall-clock time in this operator (excluding children)
    elapsed: Duration,
    /// Operator-specific details (e.g., "predicate: level = 'error'")
    details: Option<String>,
    /// Buffer/cache stats (only populated when BUFFERS requested)
    buffers: Option<BufferStats>,
    /// Child operator stats (forms a tree)
    children: Vec<OperatorStats>,
}

/// Cache and I/O stats per operator. Populated by EXPLAIN ANALYZE BUFFERS.
struct BufferStats {
    // -- Segment cache (L2, disk) --
    /// Segment cache hits (compressed segment found on disk)
    segment_cache_hits: u64,
    /// Segment cache misses (had to fetch from S3)
    segment_cache_misses: u64,

    // -- Block cache (L1, memory) --
    /// Block cache hits (decoded block found in memory)
    block_cache_hits: u64,
    /// Block cache misses (had to decode from segment)
    block_cache_misses: u64,

    // -- Bloom filters --
    /// Segments pruned by bloom filter (definitely no match)
    bloom_pruned: u64,
    /// Segments that passed bloom check (maybe match)
    bloom_passed: u64,

    // -- S3 I/O --
    /// Number of S3 GET requests
    s3_requests: u64,
    /// Total bytes fetched from S3
    s3_bytes_fetched: u64,
    /// Total time spent waiting on S3
    s3_elapsed: Duration,

    // -- Decompression --
    /// Bytes decompressed (zstd output)
    decompress_bytes: u64,
    /// Time spent in zstd decompression
    decompress_elapsed: Duration,
}
```

### EXPLAIN Modes

| Mode | What it does |
|------|-------------|
| `EXPLAIN` | Show the physical plan tree without executing. Useful for checking bloom pruning, segment selection, and plan shape. |
| `EXPLAIN ANALYZE` | Execute the query and show the plan tree annotated with actual row counts and wall-clock times per operator. |
| `EXPLAIN ANALYZE BUFFERS` | Same as ANALYZE, plus cache hit/miss rates, S3 fetch counts, decompression stats. The full picture for benchmarking. |

### EXPLAIN Output Format

Text-based tree, similar to PostgreSQL. Each node shows the operator name,
estimated/actual rows, and timing:

```
EXPLAIN ANALYZE BUFFERS SELECT * FROM api_gateway WHERE level = 'error' LIMIT 10

LimitExec (limit=10, rows=10, time=0.002ms)
  └── FilterExec (predicate=level = 'error', rows=10/4200 consumed, time=1.2ms)
        └── SegmentScan (segments=3/12 after bloom, rows=4200, time=8.5ms)
              Buffers:
                bloom: 9 pruned, 3 passed
                block cache: 2 hits, 1 miss
                segment cache: 0 hits, 1 miss
                S3: 1 request, 48KB fetched, 18ms
                decompress: 48KB → 192KB, 0.8ms
```

This format is designed so that when we later swap the executor (e.g., volcano
→ vectorized), we can run the same queries under both executors and directly
compare the per-operator timing and buffer stats.

### Physical Planning Steps

1. Load manifest for namespace (via `ManifestCache` or direct S3 read)
2. Get active segments in time range (`State::segments_in_range`)
3. Check bloom filters against equality predicates (prune segments)
4. Build `ScanUnit` list from surviving segments
5. Construct physical operator tree

### SegmentScan Execution

For each `ScanUnit`:
1. Fetch segment bytes (check block cache → segment cache → S3)
2. Parse with `segment::Reader::from_bytes()`
3. For each `ProcessedEntry`:
   a. Check pushed-down predicate against indexed fields (resolve dict IDs)
   b. If predicate references non-indexed fields, parse JSON payload
   c. If match, emit Row

### Predicate Evaluation on Entries

Indexed field predicates (e.g., `level = 'error'`):
- Use `PresenceBitmap` to check if field exists
- Use `Dictionary::get(dict_id)` to resolve value
- Compare resolved string against predicate value

Non-indexed field predicates (e.g., `message LIKE '%timeout%'`):
- Parse `entry.payload` as JSON
- Extract the field value
- Evaluate predicate

### Test Cases

End-to-end tests using `MemoryStore`:
1. Build segments with `segment::Builder`, write to `MemoryStore`
2. Create manifest entries pointing to those segments
3. Run queries, assert results

- Scan all: `SELECT * FROM ns` → all entries returned
- Indexed filter: `SELECT * FROM ns WHERE level = 'error'` → only error entries
- Time range: entries outside range excluded
- Bloom pruning: segment without matching value is skipped
- Aggregate: `SELECT level, COUNT(*) FROM ns GROUP BY level`
- Payload filter: `SELECT * FROM ns WHERE message LIKE '%timeout%'`
- EXPLAIN: returns plan tree without executing (no S3 reads for segment data)
- EXPLAIN ANALYZE: returns plan tree with actual row counts and timings
- EXPLAIN ANALYZE BUFFERS: includes cache hits/misses, S3 fetch counts, bloom stats
- Stats accuracy: `rows_produced` at each operator matches actual output count

## Chunk 5: HTTP Service + Wiring

**Goal:** Expose the query engine over HTTP in `crates/querier/`.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/query` | Execute SQL query, return JSON results |
| `GET` | `/health` | Liveness |
| `GET` | `/ready` | Readiness (storage reachable, catalog loaded) |

### Request/Response

**Normal query:**

```
POST /v1/query
Content-Type: application/json

{ "sql": "SELECT * FROM api_gateway WHERE level = 'error' LIMIT 10" }

Response 200:
{
  "columns": ["timestamp", "level", "service", "message", ...],
  "rows": [
    [1705323600000, "error", "api", "Connection timeout", ...],
    ...
  ],
  "row_count": 3,
  "elapsed_ms": 12
}
```

**EXPLAIN (plan only, no execution):**

```
{ "sql": "EXPLAIN SELECT * FROM api_gateway WHERE level = 'error' LIMIT 10" }

Response 200:
{
  "plan": "LimitExec (limit=10)\n  └── FilterExec (predicate=level = 'error')\n        └── SegmentScan (segments=3/12 after bloom)",
  "elapsed_ms": 1
}
```

**EXPLAIN ANALYZE BUFFERS (full execution stats):**

```
{ "sql": "EXPLAIN ANALYZE BUFFERS SELECT * FROM api_gateway WHERE level = 'error' LIMIT 10" }

Response 200:
{
  "plan": "LimitExec (limit=10, rows=10, time=0.002ms)\n  └── FilterExec ...",
  "elapsed_ms": 12
}
```

### Service Structure

Follows the indexer pattern:

```rust
pub struct QuerierState {
    pub storage: Arc<dyn ObjectStore>,
    pub cache: Arc<CacheManager>,
}

pub struct QuerierService {
    config: Config,
    state: Arc<QuerierState>,
}
```

### Startup Flow

1. Initialize storage backend
2. Initialize `CacheManager`
3. Load catalog (fetch from S3, populate `CatalogCache`)
4. Start HTTP server

## MVP Scope & Simplifications

These are explicit non-goals for the MVP. Each can be added incrementally later
without rearchitecting:

| Simplification | Why it's OK |
|----------------|-------------|
| L0 segments only | Steward (compaction) doesn't exist yet |
| No streaming results | Collect and return; good enough for LIMIT-bound queries |
| No NVMe segment cache | Use block cache (memory) + direct S3; add disk cache later |
| No consistent hashing | Single querier instance for MVP |
| `LIKE` does brute-force scan | Secondary full-text indexes are a Steward feature |
| No query timeout/cancellation | Add when we have real users |

## Dependency Order

```
Chunk 1 (Parser)
    │
    ▼
Chunk 2 (Logical Planner)
    │
    ▼
Chunk 3 (Optimizer)
    │
    ▼
Chunk 4 (Physical Planner + Executor)
    │
    ▼
Chunk 5 (HTTP Wiring)
```

Each chunk produces types consumed by the next. The parser is the natural
starting point — zero external dependencies, fully unit-testable.
