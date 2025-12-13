use crate::expr::types::{CountSize, PluzzeId};

#[derive(Debug, Clone)]
pub enum CmpOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

#[derive(Debug, Clone)]
pub enum SetExpr {
    // (set 1 2 3)
    Explicit(Vec<PluzzeId>),
    // (range 1 5)
    Range { start: PluzzeId, end: PluzzeId },
}

#[derive(Debug, Clone)]
pub enum GateExpr {
    And(Vec<GateExpr>),
    Or(Vec<GateExpr>),
    Not(Vec<GateExpr>),

    Completed(PluzzeId),
    AnyCompleted(SetExpr),
    NotCompleted(SetExpr),

    AllCompleted,

    CountCmp {
        op: CmpOp,
        set: SetExpr,
        n: CountSize,
    },
}
