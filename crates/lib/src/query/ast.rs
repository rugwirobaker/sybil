//! AST types for the SQL query parser.
//!
//! These types represent the parsed structure of SQL queries. All types derive
//! `Debug`, `Clone`, and `PartialEq` for testability.

use std::fmt;

/// Top-level SQL statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// A SELECT query.
    Select(SelectStatement),
    /// EXPLAIN [ANALYZE] [BUFFERS] <select>.
    Explain {
        statement: Box<Statement>,
        analyze: bool,
        buffers: bool,
    },
    /// SHOW TABLES.
    ShowTables,
    /// DESCRIBE <table>.
    Describe(String),
}

/// A SELECT statement with all its clauses.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectStatement {
    pub columns: Vec<SelectColumn>,
    pub from: From,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub order_by: Vec<OrderByExpr>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// A FROM clause source.
#[derive(Debug, Clone, PartialEq)]
pub enum From {
    /// A table (namespace) reference with an optional alias.
    Table { name: String, alias: Option<String> },
}

/// A column in the SELECT list.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectColumn {
    /// SELECT *
    Wildcard,
    /// An expression with an optional alias.
    Expr { expr: Expr, alias: Option<String> },
}

/// An expression in an ORDER BY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderByExpr {
    pub expr: Expr,
    pub ascending: bool,
}

/// Expression tree node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Integer literal.
    Integer(i64),
    /// Floating-point literal.
    Float(f64),
    /// String literal.
    StringLiteral(String),
    /// Boolean literal (TRUE / FALSE).
    Boolean(bool),
    /// Column reference.
    Column(String),
    /// Binary operation: left op right.
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    /// Unary operation: op expr.
    UnaryOp { op: UnaryOp, expr: Box<Expr> },
    /// expr IS NULL.
    IsNull(Box<Expr>),
    /// expr IS NOT NULL.
    IsNotNull(Box<Expr>),
    /// expr [NOT] IN (list).
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    /// expr [NOT] BETWEEN low AND high.
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },
    /// expr [NOT] LIKE pattern.
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
    },
    /// Function call: name(args).
    Function { name: String, args: Vec<Expr> },
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    Plus,
    Minus,
    Multiply,
    Divide,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Minus,
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Statement::Select(select) => write!(f, "{select}"),
            Statement::Explain {
                statement,
                analyze,
                buffers,
            } => {
                write!(f, "EXPLAIN")?;
                if *analyze {
                    write!(f, " ANALYZE")?;
                }
                if *buffers {
                    write!(f, " BUFFERS")?;
                }
                write!(f, " {statement}")
            }
            Statement::ShowTables => write!(f, "SHOW TABLES"),
            Statement::Describe(name) => write!(f, "DESCRIBE {name}"),
        }
    }
}

impl fmt::Display for SelectStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SELECT ")?;
        for (i, col) in self.columns.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{col}")?;
        }
        write!(f, " FROM {}", self.from)?;

        if let Some(ref w) = self.where_clause {
            write!(f, " WHERE {w}")?;
        }
        if !self.group_by.is_empty() {
            write!(f, " GROUP BY ")?;
            for (i, g) in self.group_by.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{g}")?;
            }
        }
        if !self.order_by.is_empty() {
            write!(f, " ORDER BY ")?;
            for (i, o) in self.order_by.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", o.expr)?;
                if o.ascending {
                    write!(f, " ASC")?;
                } else {
                    write!(f, " DESC")?;
                }
            }
        }
        if let Some(limit) = self.limit {
            write!(f, " LIMIT {limit}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, " OFFSET {offset}")?;
        }
        Ok(())
    }
}

impl fmt::Display for From {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            From::Table { name, alias } => {
                write!(f, "{name}")?;
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for SelectColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectColumn::Wildcard => write!(f, "*"),
            SelectColumn::Expr { expr, alias } => {
                write!(f, "{expr}")?;
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Integer(n) => write!(f, "{n}"),
            Expr::Float(n) => write!(f, "{n}"),
            Expr::StringLiteral(s) => write!(f, "'{s}'"),
            Expr::Boolean(b) => {
                if *b {
                    write!(f, "TRUE")
                } else {
                    write!(f, "FALSE")
                }
            }
            Expr::Column(name) => write!(f, "{name}"),
            Expr::BinaryOp { left, op, right } => write!(f, "({left} {op} {right})"),
            Expr::UnaryOp { op, expr } => write!(f, "{op}{expr}"),
            Expr::IsNull(expr) => write!(f, "{expr} IS NULL"),
            Expr::IsNotNull(expr) => write!(f, "{expr} IS NOT NULL"),
            Expr::InList {
                expr,
                list,
                negated,
            } => {
                write!(f, "{expr}")?;
                if *negated {
                    write!(f, " NOT")?;
                }
                write!(f, " IN (")?;
                for (i, item) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Expr::Between {
                expr,
                low,
                high,
                negated,
            } => {
                write!(f, "{expr}")?;
                if *negated {
                    write!(f, " NOT")?;
                }
                write!(f, " BETWEEN {low} AND {high}")
            }
            Expr::Like {
                expr,
                pattern,
                negated,
            } => {
                write!(f, "{expr}")?;
                if *negated {
                    write!(f, " NOT")?;
                }
                write!(f, " LIKE {pattern}")
            }
            Expr::Function { name, args } => {
                write!(f, "{name}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
        }
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOp::Eq => write!(f, "="),
            BinaryOp::Neq => write!(f, "!="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::LtEq => write!(f, "<="),
            BinaryOp::GtEq => write!(f, ">="),
            BinaryOp::And => write!(f, "AND"),
            BinaryOp::Or => write!(f, "OR"),
            BinaryOp::Plus => write!(f, "+"),
            BinaryOp::Minus => write!(f, "-"),
            BinaryOp::Multiply => write!(f, "*"),
            BinaryOp::Divide => write!(f, "/"),
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOp::Not => write!(f, "NOT "),
            UnaryOp::Minus => write!(f, "-"),
        }
    }
}
