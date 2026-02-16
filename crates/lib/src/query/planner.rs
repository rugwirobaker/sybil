//! Logical planner: converts AST into a logical plan tree.
//!
//! Pure transformation — no I/O or catalog access. Namespace resolution is
//! deferred to the physical planner.

use std::fmt;

use thiserror::Error;

use super::ast::*;

/// Errors produced during logical planning.
#[derive(Debug, Error)]
pub enum PlanError {
    #[error("SELECT with no columns")]
    EmptySelect,

    #[error("column `{0}` must appear in GROUP BY or be used in an aggregate function")]
    AggregateWithoutGroupBy(String),
}

/// Top-level plan produced by the planner.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// A query to execute.
    Query(LogicalPlan),
    /// EXPLAIN [ANALYZE] [BUFFERS] wrapping an inner plan.
    Explain {
        plan: LogicalPlan,
        analyze: bool,
        buffers: bool,
    },
    /// SHOW TABLES.
    ShowTables,
    /// DESCRIBE <table>.
    Describe(String),
}

/// A node in the logical plan tree. Children are boxed to allow recursive nesting.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    /// Table scan with optional time range pushdown.
    Scan {
        namespace: String,
        time_range: Option<(i64, i64)>,
    },
    /// Predicate filter.
    Filter {
        input: Box<LogicalPlan>,
        predicate: Expr,
    },
    /// Column projection with aliases.
    Project {
        input: Box<LogicalPlan>,
        expressions: Vec<(Expr, String)>,
    },
    /// Aggregation with GROUP BY.
    Aggregate {
        input: Box<LogicalPlan>,
        group_by: Vec<Expr>,
        aggregates: Vec<AggregateExpr>,
    },
    /// ORDER BY.
    Sort {
        input: Box<LogicalPlan>,
        order_by: Vec<SortExpr>,
    },
    /// LIMIT / OFFSET.
    Limit {
        input: Box<LogicalPlan>,
        limit: usize,
        offset: usize,
    },
}

/// An aggregate function call in the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateExpr {
    pub function: AggregateFunction,
    pub args: Vec<Expr>,
    pub alias: String,
}

/// Supported aggregate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFunction {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

impl fmt::Display for AggregateFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggregateFunction::Count => write!(f, "COUNT"),
            AggregateFunction::Sum => write!(f, "SUM"),
            AggregateFunction::Min => write!(f, "MIN"),
            AggregateFunction::Max => write!(f, "MAX"),
            AggregateFunction::Avg => write!(f, "AVG"),
        }
    }
}

/// A sort expression in the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct SortExpr {
    pub expr: Expr,
    pub ascending: bool,
}

/// Returns the corresponding `AggregateFunction` if `name` is a known aggregate.
fn parse_aggregate(name: &str) -> Option<AggregateFunction> {
    match name.to_ascii_uppercase().as_str() {
        "COUNT" => Some(AggregateFunction::Count),
        "SUM" => Some(AggregateFunction::Sum),
        "MIN" => Some(AggregateFunction::Min),
        "MAX" => Some(AggregateFunction::Max),
        "AVG" => Some(AggregateFunction::Avg),
        _ => None,
    }
}

/// Flatten top-level AND conjuncts into a list.
fn flatten_and(expr: Expr) -> Vec<Expr> {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOp::And,
            right,
        } => {
            let mut out = flatten_and(*left);
            out.extend(flatten_and(*right));
            out
        }
        other => vec![other],
    }
}

/// Re-combine a list of expressions with AND. Returns `None` if the list is empty.
fn combine_with_and(exprs: Vec<Expr>) -> Option<Expr> {
    exprs.into_iter().reduce(|left, right| Expr::BinaryOp {
        left: Box::new(left),
        op: BinaryOp::And,
        right: Box::new(right),
    })
}

/// Check whether an expression is a `timestamp` column reference.
fn is_timestamp_col(expr: &Expr) -> bool {
    matches!(expr, Expr::Column(name) if name == "timestamp")
}

/// Try to extract an integer literal value from an expression.
fn as_integer(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Integer(n) => Some(*n),
        _ => None,
    }
}

/// Extract timestamp range bounds from the WHERE clause.
///
/// Returns `(time_range, residual_predicate)`. Timestamp comparisons are
/// pulled out of the predicate; everything else is returned as the residual.
fn extract_time_range(expr: Expr) -> (Option<(i64, i64)>, Option<Expr>) {
    let conjuncts = flatten_and(expr);

    let mut lower: Option<i64> = None;
    let mut upper: Option<i64> = None;
    let mut residual = Vec::new();

    for conjunct in conjuncts {
        match &conjunct {
            // timestamp > N  /  timestamp >= N
            Expr::BinaryOp { left, op, right }
                if is_timestamp_col(left) && matches!(op, BinaryOp::Gt | BinaryOp::GtEq) =>
            {
                if let Some(n) = as_integer(right) {
                    lower = Some(lower.map_or(n, |prev: i64| prev.max(n)));
                } else {
                    residual.push(conjunct);
                }
            }
            // timestamp < N  /  timestamp <= N
            Expr::BinaryOp { left, op, right }
                if is_timestamp_col(left) && matches!(op, BinaryOp::Lt | BinaryOp::LtEq) =>
            {
                if let Some(n) = as_integer(right) {
                    upper = Some(upper.map_or(n, |prev: i64| prev.min(n)));
                } else {
                    residual.push(conjunct);
                }
            }
            // timestamp BETWEEN low AND high
            Expr::Between {
                expr,
                low,
                high,
                negated: false,
            } if is_timestamp_col(expr) => {
                if let (Some(lo), Some(hi)) = (as_integer(low), as_integer(high)) {
                    lower = Some(lower.map_or(lo, |prev: i64| prev.max(lo)));
                    upper = Some(upper.map_or(hi, |prev: i64| prev.min(hi)));
                } else {
                    residual.push(conjunct);
                }
            }
            _ => {
                residual.push(conjunct);
            }
        }
    }

    let time_range = match (lower, upper) {
        (Some(lo), Some(hi)) => Some((lo, hi)),
        (Some(lo), None) => Some((lo, i64::MAX)),
        (None, Some(hi)) => Some((i64::MIN, hi)),
        (None, None) => None,
    };

    (time_range, combine_with_and(residual))
}

/// Generate an alias string for a `SelectColumn`.
fn column_alias(col: &SelectColumn) -> String {
    match col {
        SelectColumn::Wildcard => "*".into(),
        SelectColumn::Expr { alias: Some(a), .. } => a.clone(),
        SelectColumn::Expr {
            expr: Expr::Column(name),
            ..
        } => name.clone(),
        SelectColumn::Expr {
            expr: Expr::Function { name, args },
            ..
        } => {
            let args_str: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            format!("{}({})", name, args_str.join(", "))
        }
        SelectColumn::Expr { expr, .. } => expr.to_string(),
    }
}

/// Convert a parsed [`Statement`] into a [`Plan`].
pub fn plan(statement: &Statement) -> Result<Plan, PlanError> {
    match statement {
        Statement::Select(select) => Ok(Plan::Query(plan_select(select)?)),
        Statement::Explain {
            statement: inner,
            analyze,
            buffers,
        } => {
            // EXPLAIN always wraps a SELECT — the parser enforces this.
            let Plan::Query(logical) = plan(inner)? else {
                unreachable!("parser only allows EXPLAIN SELECT");
            };
            Ok(Plan::Explain {
                plan: logical,
                analyze: *analyze,
                buffers: *buffers,
            })
        }
        Statement::ShowTables => Ok(Plan::ShowTables),
        Statement::Describe(name) => Ok(Plan::Describe(name.clone())),
    }
}

/// Plan a SELECT statement into a LogicalPlan tree (bottom-up).
fn plan_select(select: &SelectStatement) -> Result<LogicalPlan, PlanError> {
    if select.columns.is_empty() {
        return Err(PlanError::EmptySelect);
    }

    let From::Table { name, .. } = &select.from;

    // 1+2. Scan with time range extracted from WHERE, plus residual filter.
    let (time_range, residual) = select
        .where_clause
        .as_ref()
        .map(|w| extract_time_range(w.clone()))
        .unwrap_or((None, None));

    let mut node = LogicalPlan::Scan {
        namespace: name.clone(),
        time_range,
    };

    if let Some(predicate) = residual {
        node = LogicalPlan::Filter {
            input: Box::new(node),
            predicate,
        };
    }

    // 3. Aggregate or Project
    let aggregates = collect_aggregates(&select.columns);
    if !aggregates.is_empty() {
        node = LogicalPlan::Aggregate {
            input: Box::new(node),
            group_by: select.group_by.clone(),
            aggregates,
        };
    } else {
        node = plan_project(node, &select.columns);
    }

    // 4. Sort
    if !select.order_by.is_empty() {
        node = LogicalPlan::Sort {
            input: Box::new(node),
            order_by: select
                .order_by
                .iter()
                .map(|o| SortExpr {
                    expr: o.expr.clone(),
                    ascending: o.ascending,
                })
                .collect(),
        };
    }

    // 5. Limit
    if let Some(limit) = select.limit {
        node = LogicalPlan::Limit {
            input: Box::new(node),
            limit,
            offset: select.offset.unwrap_or(0),
        };
    }

    Ok(node)
}

/// Extract aggregate function calls from the SELECT column list.
fn collect_aggregates(columns: &[SelectColumn]) -> Vec<AggregateExpr> {
    columns
        .iter()
        .filter_map(|col| {
            let SelectColumn::Expr {
                expr: Expr::Function { name, args },
                ..
            } = col
            else {
                return None;
            };
            let func = parse_aggregate(name)?;
            let alias = column_alias(col);
            Some(AggregateExpr {
                function: func,
                args: args.clone(),
                alias,
            })
        })
        .collect()
}

/// Wrap the input in a Project node, or pass through for `SELECT *`.
fn plan_project(input: LogicalPlan, columns: &[SelectColumn]) -> LogicalPlan {
    if matches!(columns, [SelectColumn::Wildcard]) {
        return input;
    }

    let expressions = columns
        .iter()
        .map(|col| match col {
            SelectColumn::Expr { expr, .. } => (expr.clone(), column_alias(col)),
            SelectColumn::Wildcard => (Expr::Column("*".into()), "*".into()),
        })
        .collect();

    LogicalPlan::Project {
        input: Box::new(input),
        expressions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parser::parse;

    /// Helper: parse SQL and produce a Plan.
    fn plan_sql(sql: &str) -> Plan {
        let stmt = parse(sql).expect("parse failed");
        plan(&stmt).expect("plan failed")
    }

    // 1. Scan
    #[test]
    fn scan_select_star() {
        let p = plan_sql("SELECT * FROM ns");
        assert_eq!(
            p,
            Plan::Query(LogicalPlan::Scan {
                namespace: "ns".into(),
                time_range: None,
            })
        );
    }

    // 2. Filter
    #[test]
    fn filter_non_timestamp() {
        let p = plan_sql("SELECT * FROM ns WHERE level = 'error'");
        match p {
            Plan::Query(LogicalPlan::Filter { input, predicate }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
                assert_eq!(
                    predicate,
                    Expr::BinaryOp {
                        left: Box::new(Expr::Column("level".into())),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::StringLiteral("error".into())),
                    }
                );
            }
            other => panic!("expected Filter over Scan, got {other:?}"),
        }
    }

    // 3. Time range extraction with residual filter
    #[test]
    fn time_range_with_residual() {
        let p = plan_sql(
            "SELECT * FROM ns WHERE timestamp > 1000 AND timestamp < 2000 AND level = 'error'",
        );
        match p {
            Plan::Query(LogicalPlan::Filter { input, predicate }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: Some((1000, 2000)),
                    }
                );
                assert_eq!(
                    predicate,
                    Expr::BinaryOp {
                        left: Box::new(Expr::Column("level".into())),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::StringLiteral("error".into())),
                    }
                );
            }
            other => panic!("expected Filter over Scan with time_range, got {other:?}"),
        }
    }

    // 4. Time range only (no residual → no Filter node)
    #[test]
    fn time_range_only() {
        let p = plan_sql("SELECT * FROM ns WHERE timestamp > 1000 AND timestamp < 2000");
        assert_eq!(
            p,
            Plan::Query(LogicalPlan::Scan {
                namespace: "ns".into(),
                time_range: Some((1000, 2000)),
            })
        );
    }

    // 5. Timestamp BETWEEN
    #[test]
    fn timestamp_between() {
        let p = plan_sql("SELECT * FROM ns WHERE timestamp BETWEEN 1000 AND 2000");
        assert_eq!(
            p,
            Plan::Query(LogicalPlan::Scan {
                namespace: "ns".into(),
                time_range: Some((1000, 2000)),
            })
        );
    }

    // 6. Aggregate
    #[test]
    fn aggregate_count_with_group_by() {
        let p = plan_sql("SELECT level, COUNT(*) FROM ns GROUP BY level");
        match p {
            Plan::Query(LogicalPlan::Aggregate {
                input,
                group_by,
                aggregates,
            }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
                assert_eq!(group_by, vec![Expr::Column("level".into())]);
                assert_eq!(aggregates.len(), 1);
                assert_eq!(aggregates[0].function, AggregateFunction::Count);
                assert_eq!(aggregates[0].args, vec![Expr::Column("*".into())]);
                assert_eq!(aggregates[0].alias, "COUNT(*)");
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    // 7. Project
    #[test]
    fn project_columns() {
        let p = plan_sql("SELECT level, message FROM ns");
        match p {
            Plan::Query(LogicalPlan::Project { input, expressions }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
                assert_eq!(
                    expressions,
                    vec![
                        (Expr::Column("level".into()), "level".into()),
                        (Expr::Column("message".into()), "message".into()),
                    ]
                );
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    // 8. Sort
    #[test]
    fn sort_order_by() {
        let p = plan_sql("SELECT * FROM ns ORDER BY timestamp DESC");
        match p {
            Plan::Query(LogicalPlan::Sort { input, order_by }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
                assert_eq!(order_by.len(), 1);
                assert_eq!(order_by[0].expr, Expr::Column("timestamp".into()));
                assert!(!order_by[0].ascending);
            }
            other => panic!("expected Sort, got {other:?}"),
        }
    }

    // 9. Limit / offset
    #[test]
    fn limit_offset() {
        let p = plan_sql("SELECT * FROM ns LIMIT 100 OFFSET 50");
        match p {
            Plan::Query(LogicalPlan::Limit {
                input,
                limit,
                offset,
            }) => {
                assert_eq!(
                    *input,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
                assert_eq!(limit, 100);
                assert_eq!(offset, 50);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    // 10. Full pipeline: Filter + Sort + Limit
    #[test]
    fn full_pipeline() {
        let p = plan_sql(
            "SELECT * FROM ns WHERE timestamp > 1000 AND level = 'error' ORDER BY timestamp DESC LIMIT 50",
        );
        // Outermost: Limit
        match p {
            Plan::Query(LogicalPlan::Limit {
                input: sort_box,
                limit: 50,
                offset: 0,
            }) => {
                // Sort
                match *sort_box {
                    LogicalPlan::Sort {
                        input: filter_box,
                        ref order_by,
                    } => {
                        assert_eq!(order_by.len(), 1);
                        assert!(!order_by[0].ascending);
                        // Filter
                        match *filter_box {
                            LogicalPlan::Filter {
                                input: scan_box,
                                predicate,
                            } => {
                                assert_eq!(
                                    *scan_box,
                                    LogicalPlan::Scan {
                                        namespace: "ns".into(),
                                        time_range: Some((1000, i64::MAX)),
                                    }
                                );
                                assert_eq!(
                                    predicate,
                                    Expr::BinaryOp {
                                        left: Box::new(Expr::Column("level".into())),
                                        op: BinaryOp::Eq,
                                        right: Box::new(Expr::StringLiteral("error".into())),
                                    }
                                );
                            }
                            other => panic!("expected Filter, got {other:?}"),
                        }
                    }
                    other => panic!("expected Sort, got {other:?}"),
                }
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    // 11. EXPLAIN
    #[test]
    fn explain_wraps_plan() {
        let p = plan_sql("EXPLAIN SELECT * FROM ns");
        match p {
            Plan::Explain {
                plan: inner,
                analyze,
                buffers,
            } => {
                assert!(!analyze);
                assert!(!buffers);
                assert_eq!(
                    inner,
                    LogicalPlan::Scan {
                        namespace: "ns".into(),
                        time_range: None,
                    }
                );
            }
            other => panic!("expected Explain, got {other:?}"),
        }
    }

    #[test]
    fn explain_analyze_buffers() {
        let p = plan_sql("EXPLAIN ANALYZE BUFFERS SELECT * FROM ns");
        match p {
            Plan::Explain {
                analyze, buffers, ..
            } => {
                assert!(analyze);
                assert!(buffers);
            }
            other => panic!("expected Explain, got {other:?}"),
        }
    }

    // 12. ShowTables / Describe
    #[test]
    fn show_tables() {
        let p = plan_sql("SHOW TABLES");
        assert_eq!(p, Plan::ShowTables);
    }

    #[test]
    fn describe_table() {
        let p = plan_sql("DESCRIBE my_ns");
        assert_eq!(p, Plan::Describe("my_ns".into()));
    }

    // 13. Wildcard passthrough — no Project node
    #[test]
    fn wildcard_no_project() {
        let p = plan_sql("SELECT * FROM ns");
        // Should be a bare Scan, not wrapped in Project.
        assert!(matches!(p, Plan::Query(LogicalPlan::Scan { .. })));
    }

    // -- Additional edge cases ---

    #[test]
    fn limit_without_offset_defaults_to_zero() {
        let p = plan_sql("SELECT * FROM ns LIMIT 10");
        match p {
            Plan::Query(LogicalPlan::Limit { offset, .. }) => {
                assert_eq!(offset, 0);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_with_alias() {
        let p = plan_sql("SELECT COUNT(*) AS total FROM ns");
        match p {
            Plan::Query(LogicalPlan::Aggregate { aggregates, .. }) => {
                assert_eq!(aggregates.len(), 1);
                assert_eq!(aggregates[0].alias, "total");
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn multiple_aggregates() {
        let p = plan_sql("SELECT MIN(duration), MAX(duration), AVG(duration) FROM ns");
        match p {
            Plan::Query(LogicalPlan::Aggregate { aggregates, .. }) => {
                assert_eq!(aggregates.len(), 3);
                assert_eq!(aggregates[0].function, AggregateFunction::Min);
                assert_eq!(aggregates[1].function, AggregateFunction::Max);
                assert_eq!(aggregates[2].function, AggregateFunction::Avg);
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn timestamp_gte_and_lte() {
        let p = plan_sql("SELECT * FROM ns WHERE timestamp >= 1000 AND timestamp <= 2000");
        assert_eq!(
            p,
            Plan::Query(LogicalPlan::Scan {
                namespace: "ns".into(),
                time_range: Some((1000, 2000)),
            })
        );
    }

    #[test]
    fn project_with_alias() {
        let p = plan_sql("SELECT level AS lvl FROM ns");
        match p {
            Plan::Query(LogicalPlan::Project { expressions, .. }) => {
                assert_eq!(
                    expressions,
                    vec![(Expr::Column("level".into()), "lvl".into())]
                );
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn multiple_sort_keys() {
        let p = plan_sql("SELECT * FROM ns ORDER BY level ASC, timestamp DESC");
        match p {
            Plan::Query(LogicalPlan::Sort { order_by, .. }) => {
                assert_eq!(order_by.len(), 2);
                assert!(order_by[0].ascending);
                assert!(!order_by[1].ascending);
            }
            other => panic!("expected Sort, got {other:?}"),
        }
    }
}
