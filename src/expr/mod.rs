#![allow(unused)]

pub mod ast;
mod compiler;
mod parser;
pub mod types;

use crate::expr::{ast::GateExpr, types::PuzzleStates};

pub fn compile_gate_expr(expr: &str) -> Result<GateExpr, String> {
    let tokens = parser::tokenize(expr);
    let (sexpr, _used) = parser::parse_expr(&tokens).map_err(|e| format!("Parse Error: {e:?}"))?;
    compiler::compile_gate(&sexpr).map_err(|e| format!("Compile Error: {e:?}"))
}

/// A state-aware S-expression predicate language for gating and progression.
pub fn eval<S: PuzzleStates>(state: &S, expr: &str) -> bool {
    let expr = compile_gate_expr(expr);
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
        fn is_completed(&self, id: super::types::PuzzleId) -> bool {
            UNLOCKED.contains(&id)
        }

        fn completed_count(&self) -> super::types::CountSize {
            UNLOCKED.len()
        }

        fn completed(&self) -> Vec<super::types::PuzzleId> {
            UNLOCKED.to_vec()
        }

        fn game_started(&self) -> bool {
            true
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

        let expr = "(game-started)";
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
        assert!(!result);
    }
}
