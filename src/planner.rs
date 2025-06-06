use crate::ast::{BinOp, Datum, Expr, OptArgs, Term, UnOp};

#[derive(Debug)]
pub enum PlanError {
    UnsupportedTerm(Term),
    InvalidPredicate(Term),
    InvalidGetTerm(Term),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedTerm(term) => {
                write!(f, "Unsupported term encountered during planning: {term:?}")
            }
            Self::InvalidPredicate(term) => {
                write!(f, "Filter predicate is not a boolean expression: {term:?}")
            }
            Self::InvalidGetTerm(term) => write!(f, "Get term missing table or key: {term:?}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// Planner‐side representation of a compiled/optimized node.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanNode {
    SelectDatabase {
        name: String,
    },
    CreateDatabase {
        name: String,
    },
    DropDatabase {
        name: String,
    },
    ListDatabases,
    ScanTable {
        db: Option<String>,
        name: String,
    },
    CreateTable {
        db: Option<String>,
        name: String,
    },
    DropTable {
        db: Option<String>,
        name: String,
    },
    ListTables {
        db: Option<String>,
    },
    GetByKey {
        db: Option<String>,
        table: String,
        key: Datum,
        opt_args: OptArgs,
    },
    Filter {
        source: Box<PlanNode>,
        predicate: Expr, // raw Expr, to be simplified in optimize()
        opt_args: OptArgs,
    },
    Insert {
        table: Box<PlanNode>,
        documents: Vec<Datum>,
        opt_args: OptArgs,
    },
    Delete {
        source: Box<PlanNode>,
        opt_args: OptArgs,
    },
    Eval {
        expr: Expr,
    },
    Constant(Datum),
}

pub struct Planner;

impl Planner {
    pub const fn new() -> Self {
        Self
    }

    #[allow(clippy::only_used_in_recursion)]
    pub fn plan(&self, term: &Term) -> Result<PlanNode, PlanError> {
        match term {
            Term::Expr(e) => Ok(PlanNode::Eval { expr: e.clone() }),

            Term::Database { name } => Ok(PlanNode::SelectDatabase { name: name.clone() }),

            Term::DatabaseCreate { name } => Ok(PlanNode::CreateDatabase { name: name.clone() }),

            Term::DatabaseDrop { name } => Ok(PlanNode::DropDatabase { name: name.clone() }),

            Term::DatabaseList => Ok(PlanNode::ListDatabases),

            Term::Table { db, name } => Ok(PlanNode::ScanTable {
                db: db.clone(),
                name: name.clone(),
            }),

            Term::TableCreate { db, name } => Ok(PlanNode::CreateTable {
                db: db.clone(),
                name: name.clone(),
            }),

            Term::TableDrop { db, name } => Ok(PlanNode::DropTable {
                db: db.clone(),
                name: name.clone(),
            }),

            Term::TableList { db } => Ok(PlanNode::ListTables { db: db.clone() }),

            Term::Get {
                table,
                key,
                opt_args,
            } => {
                let (db, table) = match &**table {
                    Term::Table { db, name, .. } => (db.clone(), name.clone()),
                    _ => (None, format!("{:?}", self.plan(table)?)),
                };

                let key = match key {
                    Datum::String(s) => Datum::String(s.clone()),
                    other => {
                        return Err(PlanError::InvalidGetTerm(Term::Datum(other.clone())));
                    }
                };

                Ok(PlanNode::GetByKey {
                    db,
                    table,
                    key,
                    opt_args: opt_args.clone(),
                })
            }

            Term::Filter {
                source,
                predicate,
                opt_args,
            } => {
                if let Term::Expr(e) = predicate.as_ref() {
                    Ok(PlanNode::Filter {
                        source: Box::new(self.plan(source)?),
                        predicate: e.clone(),
                        opt_args: opt_args.clone(),
                    })
                } else {
                    Err(PlanError::InvalidPredicate(predicate.as_ref().clone()))
                }
            }

            Term::Insert {
                table,
                documents,
                opt_args,
            } => Ok(PlanNode::Insert {
                table: Box::new(self.plan(table)?),
                documents: documents.clone(),
                opt_args: opt_args.clone(),
            }),

            Term::Delete { source, opt_args } => Ok(PlanNode::Delete {
                source: Box::new(self.plan(source)?),
                opt_args: opt_args.clone(),
            }),

            Term::Datum(_) => Err(PlanError::UnsupportedTerm(term.clone())),
        }
    }

    pub fn simplify_expr(expr: Expr) -> Expr {
        match expr {
            Expr::Constant(_) | Expr::Field { .. } => expr,
            Expr::BinaryOp { op, left, right } => {
                let left_simplified = Self::simplify_expr(*left);
                let right_simplified = Self::simplify_expr(*right);

                match (&op, &left_simplified, &right_simplified) {
                    (BinOp::And, Expr::Constant(Datum::Bool(true)), r)
                    | (BinOp::Or, Expr::Constant(Datum::Bool(false)), r) => r.clone(),

                    (BinOp::And, l, Expr::Constant(Datum::Bool(true)))
                    | (BinOp::Or, l, Expr::Constant(Datum::Bool(false))) => l.clone(),

                    (BinOp::And, _, Expr::Constant(Datum::Bool(false)))
                    | (BinOp::And, Expr::Constant(Datum::Bool(false)), _) => {
                        Expr::Constant(Datum::Bool(false))
                    }

                    (BinOp::Or, Expr::Constant(Datum::Bool(true)), _)
                    | (BinOp::Or, _, Expr::Constant(Datum::Bool(true))) => {
                        Expr::Constant(Datum::Bool(true))
                    }

                    _ => Expr::BinaryOp {
                        op,
                        left: Box::new(left_simplified),
                        right: Box::new(right_simplified),
                    },
                }
            }
            Expr::UnaryOp { op, expr } => {
                let simplified_expr = Self::simplify_expr(*expr);
                match (&op, &simplified_expr) {
                    (UnOp::Not, Expr::Constant(Datum::Bool(b))) => Expr::Constant(Datum::Bool(!*b)),
                    _ => Expr::UnaryOp {
                        op,
                        expr: Box::new(simplified_expr),
                    },
                }
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub fn optimize(&self, plan: PlanNode) -> PlanNode {
        match plan {
            PlanNode::Filter {
                source,
                predicate,
                opt_args,
            } => {
                let optimized_source = self.optimize(*source);
                let simplified_pred = Self::simplify_expr(predicate);

                match &simplified_pred {
                    Expr::Constant(Datum::Bool(true)) => optimized_source,
                    Expr::Constant(Datum::Bool(false)) => PlanNode::Constant(Datum::Array(vec![])),
                    _ => PlanNode::Filter {
                        source: Box::new(optimized_source),
                        predicate: simplified_pred,
                        opt_args,
                    },
                }
            }

            PlanNode::Insert {
                table,
                documents,
                opt_args,
            } => PlanNode::Insert {
                table: Box::new(self.optimize(*table)),
                documents,
                opt_args,
            },

            PlanNode::Delete { source, opt_args } => PlanNode::Delete {
                source: Box::new(self.optimize(*source)),
                opt_args,
            },

            PlanNode::GetByKey {
                db,
                table,
                key,
                opt_args,
            } => PlanNode::GetByKey {
                db,
                table,
                key,
                opt_args,
            },

            PlanNode::Eval { expr } => {
                let simplified = Self::simplify_expr(expr);
                PlanNode::Eval { expr: simplified }
            }

            plan_node => plan_node,
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub fn explain(&self, plan: &PlanNode, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        match plan {
            PlanNode::SelectDatabase { name } => format!("{pad}SelectDatabase: {name}"),

            PlanNode::CreateDatabase { name } => format!("{pad}CreateDatabase: {name}"),

            PlanNode::DropDatabase { name } => format!("{pad}DropDatabase: {name}"),

            PlanNode::ListDatabases => format!("{pad}ListDatabases"),

            PlanNode::ScanTable { db, name } => {
                format!(
                    "{pad}ScanTable: {name}\n{}",
                    self.explain(
                        &PlanNode::SelectDatabase {
                            name: db.clone().unwrap_or_default()
                        },
                        indent + 1
                    ),
                )
            }

            PlanNode::CreateTable { name, .. } => format!("{pad}CreateTable: {name}"),

            PlanNode::DropTable { name, .. } => format!("{pad}DropTable: {name}"),

            PlanNode::ListTables { .. } => format!("{pad}ListTables"),

            PlanNode::GetByKey { table, key, .. } => {
                format!("{pad}GetByKey: table={table}, key={key:?}")
            }

            PlanNode::Filter {
                source, predicate, ..
            } => format!(
                "{pad}Filter: {predicate}\n{}",
                self.explain(source, indent + 1),
            ),

            PlanNode::Insert {
                table, documents, ..
            } => format!(
                "{}Insert {} docs\n{}",
                pad,
                documents.len(),
                self.explain(table, indent + 1)
            ),

            PlanNode::Delete { source, .. } => {
                format!("{}Delete\n{}", pad, self.explain(source, indent + 1))
            }

            PlanNode::Eval { expr } => format!("{pad}Eval: {expr}"),

            PlanNode::Constant(d) => format!("{pad}Constant: {d:?}"),
        }
    }
}
