use crate::expr::{
    PuzzleStates,
    types::{CountSize, PuzzleId},
};

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
    Explicit(Vec<PuzzleId>),
    // (range 1 5)
    Range { start: PuzzleId, end: PuzzleId },
}

#[derive(Debug, Clone)]
pub enum GateExpr {
    And(Vec<GateExpr>),
    Or(Vec<GateExpr>),
    Not(Box<GateExpr>),

    Completed(PuzzleId),
    AllCompleted(SetExpr),
    AnyCompleted(SetExpr),

    CountCmp {
        op: CmpOp,
        set: SetExpr,
        n: CountSize,
    },
}

fn cmp_usize(op: CmpOp, lhs: usize, rhs: usize) -> bool {
    match op {
        CmpOp::Gt => lhs > rhs,
        CmpOp::Ge => lhs >= rhs,
        CmpOp::Lt => lhs < rhs,
        CmpOp::Le => lhs <= rhs,
        CmpOp::Eq => lhs == rhs,
        CmpOp::Ne => lhs != rhs,
    }
}

fn cmp_f64(op: CmpOp, lhs: f64, rhs: f64) -> bool {
    match op {
        CmpOp::Gt => lhs > rhs,
        CmpOp::Ge => lhs >= rhs,
        CmpOp::Lt => lhs < rhs,
        CmpOp::Le => lhs <= rhs,
        CmpOp::Eq => (lhs - rhs).abs() <= 1e-12,
        CmpOp::Ne => (lhs - rhs).abs() > 1e-12,
    }
}

pub fn materialize_set<S: PuzzleStates>(state: &S, set: &SetExpr) -> Vec<PuzzleId> {
    match set {
        SetExpr::Explicit(v) => v.clone(),
        SetExpr::Range { start, end } => {
            let (a, b) = (*start, *end);
            if a <= b {
                (a..=b).collect()
            } else {
                (b..=a).collect()
            }
        }
    }
}

pub fn eval_compiled<S: PuzzleStates>(state: &S, expr: &GateExpr) -> bool {
    match expr {
        GateExpr::And(xs) => xs.iter().all(|e| eval_compiled(state, e)),
        GateExpr::Or(xs) => xs.iter().any(|e| eval_compiled(state, e)),
        GateExpr::Not(x) => !eval_compiled(state, x),

        GateExpr::Completed(id) => state.is_unlocked(*id),

        GateExpr::AllCompleted(set) => materialize_set(state, set)
            .iter()
            .all(|&id| state.is_unlocked(id)),
        GateExpr::AnyCompleted(set) => materialize_set(state, set)
            .iter()
            .any(|&id| state.is_unlocked(id)),

        GateExpr::CountCmp { op, set, n } => {
            let ids = materialize_set(state, set);
            let cnt = ids.into_iter().filter(|&id| state.is_unlocked(id)).count();
            cmp_usize(op.clone(), cnt, *n)
        }
    }
}
