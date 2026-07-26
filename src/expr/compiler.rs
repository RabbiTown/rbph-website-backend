use crate::expr::{
    ast::{CmpOp, GateExpr, SetExpr, ValueExpr},
    parser::RawSexpr,
    types::{PuzzleRef, RoundRef},
};

#[derive(Debug)]
pub enum CompileError {
    BadForm(&'static str),
    UnknownOp(String),
    ExpectedAtom,
    ExpectedNumber,
    ExpectedSetExpr,
    ExpectedValueExpr,
}

fn atom(s: &RawSexpr) -> Result<&str, CompileError> {
    match s {
        RawSexpr::Atom(a) => Ok(a.as_str()),
        _ => Err(CompileError::ExpectedAtom),
    }
}

fn parse_u32(s: &RawSexpr) -> Result<u32, CompileError> {
    let a = atom(s)?;
    a.parse::<u32>().map_err(|_| CompileError::ExpectedNumber)
}

fn parse_usize(s: &RawSexpr) -> Result<usize, CompileError> {
    let a = atom(s)?;
    a.parse::<usize>().map_err(|_| CompileError::ExpectedNumber)
}

fn parse_puzzle_ref(s: &RawSexpr) -> Result<PuzzleRef, CompileError> {
    let a = atom(s)?;
    Ok(match a.parse::<u32>() {
        Ok(id) => PuzzleRef::Id(id),
        Err(_) => PuzzleRef::Slug(a.to_string()),
    })
}

fn parse_round_ref(s: &RawSexpr) -> Result<RoundRef, CompileError> {
    let a = atom(s)?;
    Ok(match a.parse::<u32>() {
        Ok(id) => RoundRef::Id(id),
        Err(_) => RoundRef::Slug(a.to_string()),
    })
}

fn op_to_cmp(op: &str) -> Option<CmpOp> {
    match op {
        "gt" => Some(CmpOp::Gt),
        "ge" => Some(CmpOp::Ge),
        "lt" => Some(CmpOp::Lt),
        "le" => Some(CmpOp::Le),
        "eq" => Some(CmpOp::Eq),
        "ne" => Some(CmpOp::Ne),
        _ => None,
    }
}

pub fn compile_set(expr: &RawSexpr) -> Result<SetExpr, CompileError> {
    match expr {
        RawSexpr::List(items) => {
            if items.is_empty() {
                return Err(CompileError::BadForm("empty list"));
            }
            let head = atom(&items[0])?;
            match head {
                "puzzles" => {
                    let mut ids = Vec::new();
                    for it in &items[1..] {
                        ids.push(parse_puzzle_ref(it)?);
                    }
                    Ok(SetExpr::Puzzles(ids))
                }
                "puzzle-range" => {
                    if items.len() != 3 {
                        return Err(CompileError::BadForm("puzzle-range expects 2 args"));
                    }
                    Ok(SetExpr::PuzzleRange {
                        start: parse_u32(&items[1])?,
                        end: parse_u32(&items[2])?,
                    })
                }
                "round" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("round expects 1 arg"));
                    }
                    Ok(SetExpr::Round(parse_round_ref(&items[1])?))
                }
                _ => Err(CompileError::ExpectedSetExpr),
            }
        }
        _ => Err(CompileError::ExpectedSetExpr),
    }
}

pub fn compile_value(expr: &RawSexpr) -> Result<ValueExpr, CompileError> {
    match expr {
        RawSexpr::Atom(_) => Ok(ValueExpr::Number(parse_usize(expr)?)),
        RawSexpr::List(items) => {
            if items.is_empty() {
                return Err(CompileError::BadForm("empty list"));
            }

            let head = atom(&items[0])?;
            match head {
                "solved-count" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("solved-count expects 1 set arg"));
                    }
                    Ok(ValueExpr::SolvedCount(compile_set(&items[1])?))
                }
                _ => Err(CompileError::ExpectedValueExpr),
            }
        }
    }
}

pub fn compile_gate(expr: &RawSexpr) -> Result<GateExpr, CompileError> {
    match expr {
        RawSexpr::Atom(_) => Err(CompileError::BadForm("bare atom not allowed")),
        RawSexpr::List(items) => {
            if items.is_empty() {
                return Err(CompileError::BadForm("empty list"));
            }
            let head = atom(&items[0])?;

            match head {
                "true" => {
                    if items.len() != 1 {
                        return Err(CompileError::BadForm("true expects 0 arg"));
                    }
                    Ok(GateExpr::True)
                }
                "false" => {
                    if items.len() != 1 {
                        return Err(CompileError::BadForm("false expects 0 arg"));
                    }
                    Ok(GateExpr::False)
                }
                "and" => Ok(GateExpr::And(
                    items[1..]
                        .iter()
                        .map(compile_gate)
                        .collect::<Result<_, _>>()?,
                )),
                "or" => Ok(GateExpr::Or(
                    items[1..]
                        .iter()
                        .map(compile_gate)
                        .collect::<Result<_, _>>()?,
                )),
                "not" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("not expects 1 arg"));
                    }
                    Ok(GateExpr::Not(Box::new(compile_gate(&items[1])?)))
                }
                "solved" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("solved expects 1 arg"));
                    }
                    Ok(GateExpr::Solved(parse_puzzle_ref(&items[1])?))
                }
                "all-solved" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("all-solved expects 1 set arg"));
                    }
                    Ok(GateExpr::AllSolved(compile_set(&items[1])?))
                }
                "any-solved" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("any-solved expects 1 set arg"));
                    }
                    Ok(GateExpr::AnySolved(compile_set(&items[1])?))
                }
                "game-started" => {
                    if items.len() != 1 {
                        return Err(CompileError::BadForm("game-started expects 0 arg"));
                    }
                    Ok(GateExpr::GameStarted)
                }
                "triggered" => {
                    if items.len() != 3 {
                        return Err(CompileError::BadForm("triggered expects 2 args"));
                    }
                    let key = atom(&items[2])?;
                    let valid_key = !key.is_empty()
                        && key.len() <= 64
                        && key.chars().enumerate().all(|(index, ch)| {
                            if index == 0 {
                                ch.is_ascii_alphabetic()
                            } else {
                                ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
                            }
                        });
                    if !valid_key {
                        return Err(CompileError::BadForm("invalid trigger key"));
                    }
                    Ok(GateExpr::Triggered(
                        parse_puzzle_ref(&items[1])?,
                        key.to_string(),
                    ))
                }
                "gt" | "ge" | "lt" | "le" | "eq" | "ne" => {
                    if items.len() != 3 {
                        return Err(CompileError::BadForm("comparison expects 2 args"));
                    }
                    Ok(GateExpr::Cmp {
                        op: op_to_cmp(head).unwrap(),
                        lhs: compile_value(&items[1])?,
                        rhs: compile_value(&items[2])?,
                    })
                }

                _ => Err(CompileError::UnknownOp(head.to_string())),
            }
        }
    }
}
