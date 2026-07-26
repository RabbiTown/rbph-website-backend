use crate::expr::{
    PuzzleStates,
    types::{CountSize, PuzzleId, PuzzleRef, RoundRef},
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
    // (puzzles 1 2 3)
    Puzzles(Vec<PuzzleRef>),
    // (puzzle-range 1 5)
    PuzzleRange { start: PuzzleId, end: PuzzleId },
    // (round 1)
    Round(RoundRef),
}

#[derive(Debug, Clone)]
pub enum ValueExpr {
    SolvedCount(SetExpr),
    Number(CountSize),
}

#[derive(Debug, Clone)]
pub enum GateExpr {
    True,
    False,
    And(Vec<GateExpr>),
    Or(Vec<GateExpr>),
    Not(Box<GateExpr>),

    Solved(PuzzleRef),
    AllSolved(SetExpr),
    AnySolved(SetExpr),
    GameStarted,
    Triggered(PuzzleRef, String),

    Cmp {
        op: CmpOp,
        lhs: ValueExpr,
        rhs: ValueExpr,
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

fn resolve_puzzle<S: PuzzleStates>(state: &S, puzzle: &PuzzleRef) -> Option<PuzzleId> {
    match puzzle {
        PuzzleRef::Id(id) => Some(*id),
        PuzzleRef::Slug(slug) => state.puzzle_slug(slug),
    }
}

fn resolve_round<S: PuzzleStates>(state: &S, round: &RoundRef) -> Option<Vec<PuzzleId>> {
    match round {
        RoundRef::Id(id) => state.round_puzzles(*id),
        RoundRef::Slug(slug) => state
            .round_slug(slug)
            .and_then(|id| state.round_puzzles(id)),
    }
}

pub fn materialize_set<S: PuzzleStates>(state: &S, set: &SetExpr) -> Option<Vec<PuzzleId>> {
    match set {
        SetExpr::Puzzles(v) => v
            .iter()
            .map(|puzzle| resolve_puzzle(state, puzzle))
            .collect(),
        SetExpr::PuzzleRange { start, end } => Some({
            let (a, b) = (*start, *end);
            if a <= b {
                (a..=b).collect()
            } else {
                (b..=a).collect()
            }
        }),
        SetExpr::Round(round) => resolve_round(state, round),
    }
}

pub fn eval_value<S: PuzzleStates>(state: &S, expr: &ValueExpr) -> CountSize {
    match expr {
        ValueExpr::SolvedCount(set) => materialize_set(state, set)
            .unwrap_or_default()
            .into_iter()
            .filter(|&id| state.is_solved(id))
            .count(),
        ValueExpr::Number(n) => *n,
    }
}

pub fn eval_compiled<S: PuzzleStates>(state: &S, expr: &GateExpr) -> bool {
    match expr {
        GateExpr::True => true,
        GateExpr::False => false,
        GateExpr::And(xs) => xs.iter().all(|e| eval_compiled(state, e)),
        GateExpr::Or(xs) => xs.iter().any(|e| eval_compiled(state, e)),
        GateExpr::Not(x) => !eval_compiled(state, x),

        GateExpr::Solved(puzzle) => {
            resolve_puzzle(state, puzzle).is_some_and(|id| state.is_solved(id))
        }

        GateExpr::AllSolved(set) => {
            materialize_set(state, set).is_some_and(|ids| ids.iter().all(|&id| state.is_solved(id)))
        }
        GateExpr::AnySolved(set) => {
            materialize_set(state, set).is_some_and(|ids| ids.iter().any(|&id| state.is_solved(id)))
        }

        GateExpr::GameStarted => state.game_started(),
        GateExpr::Triggered(puzzle, key) => {
            resolve_puzzle(state, puzzle).is_some_and(|id| state.is_triggered(id, key))
        }

        GateExpr::Cmp { op, lhs, rhs } => {
            cmp_usize(op.clone(), eval_value(state, lhs), eval_value(state, rhs))
        }
    }
}
