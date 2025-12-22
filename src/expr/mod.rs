#![allow(unused)]

mod ast;
mod compiler;
mod parser;
mod types;

use crate::expr::types::PuzzleStates;

/// A state-aware S-expression predicate language for gating and progression.
pub fn eval<S: PuzzleStates>(state: &S, expr: &str) -> bool {
    let tokens = parser::tokenize(expr);
    let (sexpr, _used) = parser::parse_expr(&tokens)
        .map_err(|e| format!("parse error: {e:?}"))
        .unwrap();
    let expr = compiler::compile_gate(&sexpr).map_err(|e| format!("compile error: {e:?}"));
    

    ast::eval_compiled(state, &expr.unwrap())
}

mod test {
    use crate::expr::{
        eval,
        types::{PuzzleId, PuzzleStates},
    };

    const UNLOCKED: [PuzzleId; 3] = [1, 2, 3];

    struct TestState {}

    impl PuzzleStates for TestState {
        fn is_unlocked(&self, id: super::types::PuzzleId) -> bool {
            UNLOCKED.contains(&id)
        }

        fn unlocked_count(&self) -> super::types::CountSize {
            UNLOCKED.len()
        }

        fn unlocked() -> Vec<super::types::PuzzleId> {
            UNLOCKED.to_vec()
        }
    }

    #[test]
    pub fn test_eval() {
        let state = TestState {};

        let expr = "(and 1 2 3)";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (and 1 2) (and 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(and (countge (set 1 2 3 4 5) 1))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (counteq (set 1 2 3 4 5) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(and (counteq (range 1 3) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (counteq (range 1 3) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(not (counteq (range 4 6) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(not (counteq (set 4 5 6) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "
        (or
          (range 1 3)
          (set 4 5 6)
        )
        ";
        let result = eval(&state, expr);
        assert!(result);
    }

    #[test]
    pub fn test_eval_complex() {
        let state = TestState {};
        let expr = "(or (and 1 2) (and 3))";
        let result = eval(&state, expr);
        assert!(result);
    }

    #[test]
    pub fn test_eval_failed() {
        let state = TestState {};
        let expr = "(countge (range 4 40) 1)";
        let result = eval(&state, expr);
        assert_eq!(result, false);
    }
}
