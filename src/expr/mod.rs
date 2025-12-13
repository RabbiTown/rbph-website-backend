mod ast;
mod parser;
mod types;

use crate::expr::types::{CountSize, PluzzeId};
use std::vec::Vec;

pub trait PluzzesState {
    fn is_unlocked(&self, id: PluzzeId) -> bool;
    fn unlocked_count(&self) -> CountSize;
    fn unlocked() -> Vec<PluzzeId>;
}

/// A state-aware S-expression predicate language for gating and progression.
pub fn eval<S: PluzzesState>(state: S, expr: String) -> bool {
    true
}
