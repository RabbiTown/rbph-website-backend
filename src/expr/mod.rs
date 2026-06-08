#![allow(unused)]

pub mod ast;
mod compiler;
mod parser;
pub mod types;

use crate::expr::{ast::GateExpr, types::PuzzleStates};

pub fn compile_gate_expr(expr: &str) -> Result<GateExpr, String> {
    let tokens = parser::tokenize(expr);
    let (sexpr, used) = parser::parse_expr(&tokens).map_err(|e| format!("Parse Error: {e:?}"))?;
    if used != tokens.len() {
        return Err("Parse Error: trailing tokens".to_string());
    }
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
        fn is_solved(&self, id: super::types::PuzzleId) -> bool {
            UNLOCKED.contains(&id)
        }

        fn solved(&self) -> Vec<super::types::PuzzleId> {
            UNLOCKED.to_vec()
        }

        fn puzzle_slug(&self, slug: &str) -> Option<super::types::PuzzleId> {
            match slug {
                "intro" => Some(1),
                "alpha" => Some(2),
                "beta" => Some(3),
                "gamma" => Some(4),
                _ => None,
            }
        }

        fn round_slug(&self, slug: &str) -> Option<super::types::RoundId> {
            match slug {
                "round-one" => Some(1),
                "round-two" => Some(2),
                _ => None,
            }
        }

        fn round_puzzles(&self, id: super::types::RoundId) -> Option<Vec<super::types::PuzzleId>> {
            match id {
                1 => Some(vec![1, 2, 3]),
                2 => Some(vec![4, 5, 6]),
                _ => None,
            }
        }

        fn game_started(&self) -> bool {
            true
        }
    }

    #[test]
    pub fn test_eval() {
        let state = TestState {};

        let expr = "(and (solved 1) (solved 2) (solved 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (and (solved 1) (solved 2)) (and (solved 3)))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(and (ge (solved-count (puzzles 1 2 3 4 5)) 1))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (eq (solved-count (puzzles 1 2 3 4 5)) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(and (eq (solved-count (puzzle-range 1 3)) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(or (eq (solved-count (puzzle-range 1 3)) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(not (eq (solved-count (puzzle-range 4 6)) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(not (eq (solved-count (puzzles 4 5 6)) 3))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(game-started)";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "
        (or
          (any-solved (puzzle-range 1 3))
          (any-solved (puzzles 4 5 6))
        )
        ";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(ge (solved-count (round 1)) 3)";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(and (solved intro) (all-solved (puzzles intro alpha beta)))";
        let result = eval(&state, expr);
        assert!(result);

        let expr = "(ge (solved-count (round round-one)) 3)";
        let result = eval(&state, expr);
        assert!(result);
    }

    #[test]
    pub fn test_eval_complex() {
        let state = TestState {};
        let expr = "(or (and (solved 1) (solved 2)) (and (solved 3)))";
        let result = eval(&state, expr);
        assert!(result);
    }

    #[test]
    pub fn test_eval_failed() {
        let state = TestState {};
        let expr = "(ge (solved-count (puzzle-range 4 40)) 1)";
        let result = eval(&state, expr);
        assert!(!result);
    }

    #[test]
    pub fn test_bare_number_failed() {
        assert!(super::compile_gate_expr("1").is_err());
    }

    #[test]
    pub fn test_bare_set_failed() {
        assert!(super::compile_gate_expr("(puzzles 1 2 3)").is_err());
        assert!(super::compile_gate_expr("(puzzle-range 1 3)").is_err());
        assert!(super::compile_gate_expr("(round 1)").is_err());
    }

    #[test]
    pub fn test_old_names_failed() {
        assert!(super::compile_gate_expr("(completed 1)").is_err());
        assert!(super::compile_gate_expr("(all-completed (puzzles 1 2))").is_err());
        assert!(super::compile_gate_expr("(any-completed (puzzles 1 2))").is_err());
        assert!(super::compile_gate_expr("(solved-ge (puzzles 1 2) 2)").is_err());
        assert!(super::compile_gate_expr("(countge (puzzles 1 2) 2)").is_err());
        assert!(super::compile_gate_expr("(ge (solved-count (set 1 2)) 2)").is_err());
        assert!(super::compile_gate_expr("(ge (solved-count (range 1 2)) 2)").is_err());
    }

    #[test]
    pub fn test_unknown_slug_failed() {
        let state = TestState {};
        assert!(!eval(&state, "(solved unknown-puzzle)"));
        assert!(!eval(&state, "(all-solved (puzzles intro unknown-puzzle))"));
        assert!(!eval(&state, "(all-solved (round unknown-round))"));
        assert!(!eval(&state, "(ge (solved-count (round unknown-round)) 1)"));
    }
}
