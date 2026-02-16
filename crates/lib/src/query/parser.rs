//! Recursive descent SQL parser.
//!
//! Consumes tokens from the lexer and builds an AST.

use thiserror::Error;

use super::ast::*;
use super::lexer::{LexError, Lexer, Token};

/// Errors produced during parsing.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unexpected token: expected {expected}, found {found} at position {position}")]
    UnexpectedToken {
        expected: String,
        found: String,
        position: usize,
    },

    #[error("unexpected end of input: expected {expected}")]
    UnexpectedEof { expected: String },

    #[error(transparent)]
    Lex(#[from] LexError),
}

/// Result type for parse operations.
pub type Result<T> = std::result::Result<T, ParseError>;

struct Parser {
    lexer: Lexer,
    current: Token,
    current_pos: usize,
}

impl Parser {
    fn new(input: &str) -> Result<Self> {
        let mut lexer = Lexer::new(input);
        let current_pos = lexer.position();
        let current = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            current_pos,
        })
    }

    /// Advance to the next token, returning the previous one.
    fn advance(&mut self) -> Result<Token> {
        self.current_pos = self.lexer.position();
        let next = self.lexer.next_token()?;
        let prev = std::mem::replace(&mut self.current, next);
        Ok(prev)
    }

    /// Consume the current token if its discriminant matches `expected`.
    fn expect(&mut self, expected: &Token) -> Result<Token> {
        if std::mem::discriminant(&self.current) == std::mem::discriminant(expected) {
            self.advance()
        } else {
            Err(self.unexpected(&expected.describe()))
        }
    }

    /// Build an `UnexpectedToken` or `UnexpectedEof` error.
    fn unexpected(&self, expected: &str) -> ParseError {
        if self.current == Token::Eof {
            ParseError::UnexpectedEof {
                expected: expected.to_string(),
            }
        } else {
            ParseError::UnexpectedToken {
                expected: expected.to_string(),
                found: self.current.describe(),
                position: self.current_pos,
            }
        }
    }
}

impl Parser {
    fn parse_statement(&mut self) -> Result<Statement> {
        match &self.current {
            Token::Select => {
                let select = self.parse_select()?;
                Ok(Statement::Select(select))
            }
            Token::Explain => {
                self.advance()?;
                let mut analyze = false;
                let mut buffers = false;
                if self.current == Token::Analyze {
                    analyze = true;
                    self.advance()?;
                    if self.current == Token::Buffers {
                        buffers = true;
                        self.advance()?;
                    }
                }
                if self.current != Token::Select {
                    return Err(self.unexpected("SELECT after EXPLAIN"));
                }
                let select = self.parse_select()?;
                Ok(Statement::Explain {
                    statement: Box::new(Statement::Select(select)),
                    analyze,
                    buffers,
                })
            }
            Token::Show => {
                self.advance()?;
                self.expect(&Token::Tables)?;
                Ok(Statement::ShowTables)
            }
            Token::Describe => {
                self.advance()?;
                let name = self.expect_ident("table name")?;
                Ok(Statement::Describe(name))
            }
            _ => Err(self.unexpected("SELECT, EXPLAIN, SHOW, or DESCRIBE")),
        }
    }

    fn parse_select(&mut self) -> Result<SelectStatement> {
        self.expect(&Token::Select)?;
        let columns = self.parse_select_columns()?;
        self.expect(&Token::From)?;
        let from = self.parse_from()?;

        let where_clause = if self.current == Token::Where {
            self.advance()?;
            Some(self.parse_expr()?)
        } else {
            None
        };

        let group_by = if self.current == Token::Group {
            self.advance()?;
            self.expect(&Token::By)?;
            self.parse_expr_list()?
        } else {
            vec![]
        };

        let order_by = if self.current == Token::Order {
            self.advance()?;
            self.expect(&Token::By)?;
            self.parse_order_by_list()?
        } else {
            vec![]
        };

        let limit = if self.current == Token::Limit {
            self.advance()?;
            Some(self.expect_usize("LIMIT value")?)
        } else {
            None
        };

        let offset = if self.current == Token::Offset {
            self.advance()?;
            Some(self.expect_usize("OFFSET value")?)
        } else {
            None
        };

        Ok(SelectStatement {
            columns,
            from,
            where_clause,
            group_by,
            order_by,
            limit,
            offset,
        })
    }

    fn parse_select_columns(&mut self) -> Result<Vec<SelectColumn>> {
        if self.current == Token::Star {
            self.advance()?;
            return Ok(vec![SelectColumn::Wildcard]);
        }

        let mut columns = vec![self.parse_select_column()?];
        while self.current == Token::Comma {
            self.advance()?;
            columns.push(self.parse_select_column()?);
        }
        Ok(columns)
    }

    fn parse_select_column(&mut self) -> Result<SelectColumn> {
        let expr = self.parse_expr()?;
        let alias = if self.current == Token::As {
            self.advance()?;
            Some(self.expect_ident("alias")?)
        } else {
            None
        };
        Ok(SelectColumn::Expr { expr, alias })
    }

    fn parse_from(&mut self) -> Result<From> {
        let name = self.expect_ident("table name")?;
        let alias = if self.current == Token::As {
            self.advance()?;
            Some(self.expect_ident("table alias")?)
        } else {
            None
        };
        Ok(From::Table { name, alias })
    }

    fn parse_order_by_list(&mut self) -> Result<Vec<OrderByExpr>> {
        let mut list = vec![self.parse_order_by_expr()?];
        while self.current == Token::Comma {
            self.advance()?;
            list.push(self.parse_order_by_expr()?);
        }
        Ok(list)
    }

    fn parse_order_by_expr(&mut self) -> Result<OrderByExpr> {
        let expr = self.parse_expr()?;
        let ascending = if self.current == Token::Desc {
            self.advance()?;
            false
        } else if self.current == Token::Asc {
            self.advance()?;
            true
        } else {
            true // default ascending
        };
        Ok(OrderByExpr { expr, ascending })
    }

    fn parse_expr_list(&mut self) -> Result<Vec<Expr>> {
        let mut list = vec![self.parse_expr()?];
        while self.current == Token::Comma {
            self.advance()?;
            list.push(self.parse_expr()?);
        }
        Ok(list)
    }
}

// Expression parsing (precedence climbing)
impl Parser {
    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.current == Token::Or {
            self.advance()?;
            let right = self.parse_and()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        while self.current == Token::And {
            self.advance()?;
            let right = self.parse_not()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        if self.current == Token::Not {
            self.advance()?;
            let expr = self.parse_not()?;
            Ok(Expr::UnaryOp {
                op: UnaryOp::Not,
                expr: Box::new(expr),
            })
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let left = self.parse_addition()?;

        match &self.current {
            Token::Eq => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::Eq,
                    right: Box::new(right),
                })
            }
            Token::Neq => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::Neq,
                    right: Box::new(right),
                })
            }
            Token::Lt => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::Lt,
                    right: Box::new(right),
                })
            }
            Token::Gt => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::Gt,
                    right: Box::new(right),
                })
            }
            Token::LtEq => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::LtEq,
                    right: Box::new(right),
                })
            }
            Token::GtEq => {
                self.advance()?;
                let right = self.parse_addition()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOp::GtEq,
                    right: Box::new(right),
                })
            }
            Token::Is => {
                self.advance()?;
                if self.current == Token::Not {
                    self.advance()?;
                    self.expect(&Token::Null)?;
                    Ok(Expr::IsNotNull(Box::new(left)))
                } else {
                    self.expect(&Token::Null)?;
                    Ok(Expr::IsNull(Box::new(left)))
                }
            }
            Token::Not => {
                self.advance()?;
                match &self.current {
                    Token::In => {
                        self.advance()?;
                        self.expect(&Token::LParen)?;
                        let list = self.parse_expr_list()?;
                        self.expect(&Token::RParen)?;
                        Ok(Expr::InList {
                            expr: Box::new(left),
                            list,
                            negated: true,
                        })
                    }
                    Token::Between => {
                        self.advance()?;
                        let low = self.parse_addition()?;
                        self.expect(&Token::And)?;
                        let high = self.parse_addition()?;
                        Ok(Expr::Between {
                            expr: Box::new(left),
                            low: Box::new(low),
                            high: Box::new(high),
                            negated: true,
                        })
                    }
                    Token::Like => {
                        self.advance()?;
                        let pattern = self.parse_addition()?;
                        Ok(Expr::Like {
                            expr: Box::new(left),
                            pattern: Box::new(pattern),
                            negated: true,
                        })
                    }
                    _ => Err(self.unexpected("IN, BETWEEN, or LIKE after NOT")),
                }
            }
            Token::In => {
                self.advance()?;
                self.expect(&Token::LParen)?;
                let list = self.parse_expr_list()?;
                self.expect(&Token::RParen)?;
                Ok(Expr::InList {
                    expr: Box::new(left),
                    list,
                    negated: false,
                })
            }
            Token::Between => {
                self.advance()?;
                let low = self.parse_addition()?;
                self.expect(&Token::And)?;
                let high = self.parse_addition()?;
                Ok(Expr::Between {
                    expr: Box::new(left),
                    low: Box::new(low),
                    high: Box::new(high),
                    negated: false,
                })
            }
            Token::Like => {
                self.advance()?;
                let pattern = self.parse_addition()?;
                Ok(Expr::Like {
                    expr: Box::new(left),
                    pattern: Box::new(pattern),
                    negated: false,
                })
            }
            _ => Ok(left),
        }
    }

    fn parse_addition(&mut self) -> Result<Expr> {
        let mut left = self.parse_multiplication()?;
        loop {
            let op = match &self.current {
                Token::Plus => BinaryOp::Plus,
                Token::Minus => BinaryOp::Minus,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_multiplication()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match &self.current {
                Token::Star => BinaryOp::Multiply,
                Token::Slash => BinaryOp::Divide,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_unary()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        match &self.current {
            Token::Minus => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Minus,
                    expr: Box::new(expr),
                })
            }
            Token::Not => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Not,
                    expr: Box::new(expr),
                })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        match &self.current {
            Token::Integer(_) => {
                if let Token::Integer(n) = self.advance()? {
                    Ok(Expr::Integer(n))
                } else {
                    unreachable!()
                }
            }
            Token::Float(_) => {
                if let Token::Float(n) = self.advance()? {
                    Ok(Expr::Float(n))
                } else {
                    unreachable!()
                }
            }
            Token::StringLiteral(_) => {
                if let Token::StringLiteral(s) = self.advance()? {
                    Ok(Expr::StringLiteral(s))
                } else {
                    unreachable!()
                }
            }
            Token::True => {
                self.advance()?;
                Ok(Expr::Boolean(true))
            }
            Token::False => {
                self.advance()?;
                Ok(Expr::Boolean(false))
            }
            Token::Ident(_) => {
                if let Token::Ident(name) = self.advance()? {
                    // Function call: name(...)
                    if self.current == Token::LParen {
                        self.advance()?;
                        let args = if self.current == Token::RParen {
                            vec![]
                        } else if self.current == Token::Star {
                            // COUNT(*) special case
                            self.advance()?;
                            vec![Expr::Column("*".into())]
                        } else {
                            self.parse_expr_list()?
                        };
                        self.expect(&Token::RParen)?;
                        Ok(Expr::Function { name, args })
                    } else {
                        Ok(Expr::Column(name))
                    }
                } else {
                    unreachable!()
                }
            }
            Token::LParen => {
                self.advance()?;
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            _ => Err(self.unexpected("expression")),
        }
    }

    fn expect_ident(&mut self, context: &str) -> Result<String> {
        if matches!(self.current, Token::Ident(_)) {
            if let Token::Ident(name) = self.advance()? {
                Ok(name)
            } else {
                unreachable!()
            }
        } else {
            Err(self.unexpected(context))
        }
    }

    fn expect_usize(&mut self, context: &str) -> Result<usize> {
        if let Token::Integer(n) = &self.current {
            if *n >= 0 {
                let val = *n as usize;
                self.advance()?;
                return Ok(val);
            }
        }
        Err(self.unexpected(context))
    }
}

/// Parse a SQL string into a [`Statement`] AST.
pub fn parse(sql: &str) -> Result<Statement> {
    let mut parser = Parser::new(sql)?;
    let stmt = parser.parse_statement()?;

    // Allow optional trailing semicolon.
    if parser.current == Token::Semicolon {
        parser.advance()?;
    }

    if parser.current != Token::Eof {
        return Err(parser.unexpected("end of input"));
    }

    Ok(stmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Basic SELECT

    #[test]
    fn select_star() {
        let stmt = parse("SELECT * FROM api_gateway").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.columns, vec![SelectColumn::Wildcard]);
                assert_eq!(
                    s.from,
                    From::Table {
                        name: "api_gateway".into(),
                        alias: None,
                    }
                );
                assert!(s.where_clause.is_none());
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn select_columns() {
        let stmt = parse("SELECT timestamp, level, message FROM api_gateway").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.columns.len(), 3);
                assert_eq!(
                    s.columns[0],
                    SelectColumn::Expr {
                        expr: Expr::Column("timestamp".into()),
                        alias: None,
                    }
                );
                assert_eq!(
                    s.columns[1],
                    SelectColumn::Expr {
                        expr: Expr::Column("level".into()),
                        alias: None,
                    }
                );
                assert_eq!(
                    s.columns[2],
                    SelectColumn::Expr {
                        expr: Expr::Column("message".into()),
                        alias: None,
                    }
                );
                assert_eq!(
                    s.from,
                    From::Table {
                        name: "api_gateway".into(),
                        alias: None,
                    }
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn select_with_alias() {
        let stmt = parse("SELECT COUNT(*) AS total FROM ns").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.columns[0],
                    SelectColumn::Expr {
                        expr: Expr::Function {
                            name: "COUNT".into(),
                            args: vec![Expr::Column("*".into())],
                        },
                        alias: Some("total".into()),
                    }
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn select_from_with_alias() {
        let stmt = parse("SELECT * FROM api_gateway AS gw").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.from,
                    From::Table {
                        name: "api_gateway".into(),
                        alias: Some("gw".into()),
                    }
                );
            }
            _ => panic!("expected Select"),
        }
    }

    // -- WHERE clause

    #[test]
    fn where_eq() {
        let stmt = parse("SELECT * FROM api_gateway WHERE level = 'error'").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::Column("level".into())),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::StringLiteral("error".into())),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_and() {
        let stmt =
            parse("SELECT * FROM ns WHERE timestamp > 1705323600000 AND timestamp < 1705327200000")
                .unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::BinaryOp {
                            left: Box::new(Expr::Column("timestamp".into())),
                            op: BinaryOp::Gt,
                            right: Box::new(Expr::Integer(1705323600000)),
                        }),
                        op: BinaryOp::And,
                        right: Box::new(Expr::BinaryOp {
                            left: Box::new(Expr::Column("timestamp".into())),
                            op: BinaryOp::Lt,
                            right: Box::new(Expr::Integer(1705327200000)),
                        }),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_in_list() {
        let stmt =
            parse("SELECT * FROM ns WHERE level = 'error' AND service IN ('api', 'web')").unwrap();
        match stmt {
            Statement::Select(s) => {
                let w = s.where_clause.unwrap();
                match w {
                    Expr::BinaryOp {
                        op: BinaryOp::And,
                        right,
                        ..
                    } => {
                        assert_eq!(
                            *right,
                            Expr::InList {
                                expr: Box::new(Expr::Column("service".into())),
                                list: vec![
                                    Expr::StringLiteral("api".into()),
                                    Expr::StringLiteral("web".into()),
                                ],
                                negated: false,
                            }
                        );
                    }
                    _ => panic!("expected AND"),
                }
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_like() {
        let stmt = parse("SELECT * FROM ns WHERE message LIKE '%timeout%'").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::Like {
                        expr: Box::new(Expr::Column("message".into())),
                        pattern: Box::new(Expr::StringLiteral("%timeout%".into())),
                        negated: false,
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_is_null() {
        let stmt = parse("SELECT * FROM ns WHERE level IS NULL").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::IsNull(Box::new(Expr::Column("level".into()))))
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_is_not_null() {
        let stmt = parse("SELECT * FROM ns WHERE level IS NOT NULL").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::IsNotNull(Box::new(Expr::Column("level".into()))))
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_between() {
        let stmt = parse("SELECT * FROM ns WHERE ts BETWEEN 100 AND 200").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::Between {
                        expr: Box::new(Expr::Column("ts".into())),
                        low: Box::new(Expr::Integer(100)),
                        high: Box::new(Expr::Integer(200)),
                        negated: false,
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_not_in() {
        let stmt = parse("SELECT * FROM ns WHERE level NOT IN ('debug', 'trace')").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::InList {
                        expr: Box::new(Expr::Column("level".into())),
                        list: vec![
                            Expr::StringLiteral("debug".into()),
                            Expr::StringLiteral("trace".into()),
                        ],
                        negated: true,
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_not_like() {
        let stmt = parse("SELECT * FROM ns WHERE message NOT LIKE '%debug%'").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::Like {
                        expr: Box::new(Expr::Column("message".into())),
                        pattern: Box::new(Expr::StringLiteral("%debug%".into())),
                        negated: true,
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn where_not_between() {
        let stmt = parse("SELECT * FROM ns WHERE ts NOT BETWEEN 100 AND 200").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::Between {
                        expr: Box::new(Expr::Column("ts".into())),
                        low: Box::new(Expr::Integer(100)),
                        high: Box::new(Expr::Integer(200)),
                        negated: true,
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    // -- GROUP BY / ORDER BY / LIMIT / OFFSET

    #[test]
    fn group_by() {
        let stmt = parse("SELECT level, COUNT(*) FROM ns GROUP BY level").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.columns.len(), 2);
                assert_eq!(
                    s.columns[1],
                    SelectColumn::Expr {
                        expr: Expr::Function {
                            name: "COUNT".into(),
                            args: vec![Expr::Column("*".into())],
                        },
                        alias: None,
                    }
                );
                assert_eq!(s.group_by, vec![Expr::Column("level".into())]);
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn order_by_limit() {
        let stmt = parse("SELECT * FROM ns ORDER BY timestamp DESC LIMIT 100").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.order_by.len(), 1);
                assert_eq!(
                    s.order_by[0],
                    OrderByExpr {
                        expr: Expr::Column("timestamp".into()),
                        ascending: false,
                    }
                );
                assert_eq!(s.limit, Some(100));
                assert!(s.offset.is_none());
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn order_by_limit_offset() {
        let stmt = parse("SELECT * FROM ns ORDER BY timestamp DESC LIMIT 100 OFFSET 50").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.limit, Some(100));
                assert_eq!(s.offset, Some(50));
            }
            _ => panic!("expected Select"),
        }
    }

    // -- Metadata statements --
    #[test]
    fn show_tables() {
        let stmt = parse("SHOW TABLES").unwrap();
        assert_eq!(stmt, Statement::ShowTables);
    }

    #[test]
    fn describe() {
        let stmt = parse("DESCRIBE api_gateway").unwrap();
        assert_eq!(stmt, Statement::Describe("api_gateway".into()));
    }

    // -- EXPLAIN --

    #[test]
    fn explain() {
        let stmt = parse("EXPLAIN SELECT * FROM ns WHERE level = 'error'").unwrap();
        match stmt {
            Statement::Explain {
                analyze, buffers, ..
            } => {
                assert!(!analyze);
                assert!(!buffers);
            }
            _ => panic!("expected Explain"),
        }
    }

    #[test]
    fn explain_analyze() {
        let stmt = parse("EXPLAIN ANALYZE SELECT * FROM ns").unwrap();
        match stmt {
            Statement::Explain {
                analyze, buffers, ..
            } => {
                assert!(analyze);
                assert!(!buffers);
            }
            _ => panic!("expected Explain"),
        }
    }

    #[test]
    fn explain_analyze_buffers() {
        let stmt = parse("EXPLAIN ANALYZE BUFFERS SELECT * FROM ns").unwrap();
        match stmt {
            Statement::Explain {
                analyze, buffers, ..
            } => {
                assert!(analyze);
                assert!(buffers);
            }
            _ => panic!("expected Explain"),
        }
    }

    // -- Expression features --
    #[test]
    fn parenthesized_expr() {
        let stmt = parse("SELECT * FROM ns WHERE (level = 'error' OR level = 'warn')").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::BinaryOp {
                            left: Box::new(Expr::Column("level".into())),
                            op: BinaryOp::Eq,
                            right: Box::new(Expr::StringLiteral("error".into())),
                        }),
                        op: BinaryOp::Or,
                        right: Box::new(Expr::BinaryOp {
                            left: Box::new(Expr::Column("level".into())),
                            op: BinaryOp::Eq,
                            right: Box::new(Expr::StringLiteral("warn".into())),
                        }),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn boolean_literals() {
        let stmt = parse("SELECT * FROM ns WHERE active = TRUE").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::Column("active".into())),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::Boolean(true)),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn unary_minus() {
        let stmt = parse("SELECT * FROM ns WHERE value > -1").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::Column("value".into())),
                        op: BinaryOp::Gt,
                        right: Box::new(Expr::UnaryOp {
                            op: UnaryOp::Minus,
                            expr: Box::new(Expr::Integer(1)),
                        }),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn function_no_args() {
        let stmt = parse("SELECT NOW() FROM ns").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.columns[0],
                    SelectColumn::Expr {
                        expr: Expr::Function {
                            name: "NOW".into(),
                            args: vec![],
                        },
                        alias: None,
                    }
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn trailing_semicolon() {
        let stmt = parse("SHOW TABLES;").unwrap();
        assert_eq!(stmt, Statement::ShowTables);
    }

    // -- Precedence ----------------------------------------------------------

    #[test]
    fn arithmetic_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let stmt = parse("SELECT * FROM ns WHERE x = 1 + 2 * 3").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(
                    s.where_clause,
                    Some(Expr::BinaryOp {
                        left: Box::new(Expr::Column("x".into())),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::BinaryOp {
                            left: Box::new(Expr::Integer(1)),
                            op: BinaryOp::Plus,
                            right: Box::new(Expr::BinaryOp {
                                left: Box::new(Expr::Integer(2)),
                                op: BinaryOp::Multiply,
                                right: Box::new(Expr::Integer(3)),
                            }),
                        }),
                    })
                );
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn and_or_precedence() {
        // a OR b AND c should parse as a OR (b AND c)
        let stmt = parse("SELECT * FROM ns WHERE a = 1 OR b = 2 AND c = 3").unwrap();
        match stmt {
            Statement::Select(s) => {
                let w = s.where_clause.unwrap();
                match w {
                    Expr::BinaryOp {
                        op: BinaryOp::Or, ..
                    } => {} // correct: OR at top
                    _ => panic!("expected OR at top level, got {w:?}"),
                }
            }
            _ => panic!("expected Select"),
        }
    }

    // -- Display / round-trip ------------------------------------------------

    #[test]
    fn display_select() {
        let stmt = parse("SELECT * FROM api_gateway").unwrap();
        assert_eq!(stmt.to_string(), "SELECT * FROM api_gateway");
    }

    #[test]
    fn display_explain() {
        let stmt = parse("EXPLAIN ANALYZE BUFFERS SELECT * FROM ns").unwrap();
        assert_eq!(stmt.to_string(), "EXPLAIN ANALYZE BUFFERS SELECT * FROM ns");
    }

    #[test]
    fn display_show_tables() {
        let stmt = parse("SHOW TABLES").unwrap();
        assert_eq!(stmt.to_string(), "SHOW TABLES");
    }

    #[test]
    fn display_describe() {
        let stmt = parse("DESCRIBE my_table").unwrap();
        assert_eq!(stmt.to_string(), "DESCRIBE my_table");
    }

    // -- Error cases ---------------------------------------------------------

    #[test]
    fn error_missing_from() {
        let result = parse("SELECT *");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnexpectedEof { .. }
        ));
    }

    #[test]
    fn error_unclosed_paren() {
        let result = parse("SELECT * FROM ns WHERE level IN ('a'");
        assert!(result.is_err());
    }

    #[test]
    fn error_invalid_token() {
        let result = parse("SELECT * FROM ns WHERE @");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::Lex(LexError::UnexpectedChar { ch: '@', .. })
        ));
    }

    #[test]
    fn error_explain_without_select() {
        let result = parse("EXPLAIN SHOW TABLES");
        assert!(result.is_err());
    }

    #[test]
    fn error_empty_input() {
        let result = parse("");
        assert!(result.is_err());
    }

    #[test]
    fn error_trailing_garbage() {
        let result = parse("SELECT * FROM ns GARBAGE");
        assert!(result.is_err());
    }
}
