#![allow(unused)]

mod ast;
mod compiler;
mod parser;
mod types;

use crate::expr::types::PluzzesState;

/// A state-aware S-expression predicate language for gating and progression.
pub fn eval<S: PluzzesState>(state: S, expr: String) -> bool {
    let tokens = parser::tokenize(expr);
    let (sexpr, _used) = parser::parse_expr(&tokens)
        .map_err(|e| format!("parse error: {e:?}"))
        .unwrap();
    let expr = compiler::compile_gate(&sexpr).map_err(|e| format!("compile error: {e:?}"));
    let ok = ast::eval_compiled(&state, &expr.unwrap());

    ok
}

mod test {
    use crate::expr::{
        eval,
        types::{PluzzeId, PluzzesState},
    };

    const UNLOCKED: [PluzzeId; 3] = [1, 2, 3];

    struct TestState {}

    impl PluzzesState for TestState {
        fn is_unlocked(&self, id: super::types::PluzzeId) -> bool {
            UNLOCKED.contains(&id)
        }

        fn unlocked_count(&self) -> super::types::CountSize {
            UNLOCKED.len()
        }

        fn unlocked() -> Vec<super::types::PluzzeId> {
            UNLOCKED.to_vec()
        }
    }

    #[test]
    pub fn text_eval() {
        let state = TestState {};
        let expr = "(and 1 2 3)".to_string();
        let result = eval(state, expr);

        assert!(result)
    }
}
