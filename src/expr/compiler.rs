use crate::expr::{
    ast::{CmpOp, GateExpr, SetExpr},
    parser::RawSexpr,
};

#[derive(Debug)]
pub enum CompileError {
    BadForm(&'static str),
    UnknownOp(String),
    ExpectedAtom,
    ExpectedNumber,
    ExpectedSetExpr,
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

fn parse_f64(s: &RawSexpr) -> Result<f64, CompileError> {
    let a = atom(s)?;
    a.parse::<f64>().map_err(|_| CompileError::ExpectedNumber)
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
                "set" => {
                    let mut ids = Vec::new();
                    for it in &items[1..] {
                        ids.push(parse_u32(it)?);
                    }
                    Ok(SetExpr::Explicit(ids))
                }
                "range" => {
                    if items.len() != 3 {
                        return Err(CompileError::BadForm("set-range expects 2 args"));
                    }
                    Ok(SetExpr::Range {
                        start: parse_u32(&items[1])?,
                        end: parse_u32(&items[2])?,
                    })
                }
                _ => Err(CompileError::ExpectedSetExpr),
            }
        }
        _ => Err(CompileError::ExpectedSetExpr),
    }
}

pub fn compile_gate(expr: &RawSexpr) -> Result<GateExpr, CompileError> {
    match expr {
        RawSexpr::Atom(a) => {
            if let Ok(id) = a.parse::<u32>() {
                return Ok(GateExpr::Completed(id));
            }
            Err(CompileError::BadForm(
                "bare atom not allowed (except numbers)",
            ))
        }
        RawSexpr::List(items) => {
            if items.is_empty() {
                return Err(CompileError::BadForm("empty list"));
            }
            let head = atom(&items[0])?;

            match head {
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
                "completed" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("completed expects 1 arg"));
                    }
                    Ok(GateExpr::Completed(parse_u32(&items[1])?))
                }
                "all-completed" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("all-completed expects 1 set arg"));
                    }
                    Ok(GateExpr::AllCompleted(compile_set(&items[1])?))
                }
                "any-completed" => {
                    if items.len() != 2 {
                        return Err(CompileError::BadForm("any-completed expects 1 set arg"));
                    }
                    Ok(GateExpr::AnyCompleted(compile_set(&items[1])?))
                }
                "set" | "range" => Ok(GateExpr::AnyCompleted(compile_set(expr)?)),
                _ if head.starts_with("count") => {
                    let suffix = head.strip_prefix("count").unwrap();
                    let op = op_to_cmp(suffix)
                        .ok_or_else(|| CompileError::UnknownOp(head.to_string()))?;
                    if items.len() != 3 {
                        return Err(CompileError::BadForm("count* expects (set-expr n)"));
                    }
                    Ok(GateExpr::CountCmp {
                        op,
                        set: compile_set(&items[1])?,
                        n: parse_usize(&items[2])?,
                    })
                }

                _ => Err(CompileError::UnknownOp(head.to_string())),
            }
        }
    }
}
