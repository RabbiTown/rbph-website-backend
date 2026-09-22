#![allow(unused)]

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

use once_cell::sync::Lazy;

pub mod ast;
mod compiler;
mod parser;
pub mod types;

use crate::expr::{
    ast::{GateExpr, HintDisplayExpr},
    types::PuzzleStates,
};

const COMPILE_CACHE_CAPACITY: usize = 4096;

struct CompileCache<T> {
    values: HashMap<String, Result<Arc<T>, Arc<str>>>,
    insertion_order: VecDeque<String>,
}

impl<T> CompileCache<T> {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    fn get(&self, expression: &str) -> Option<Result<Arc<T>, Arc<str>>> {
        self.values.get(expression).cloned()
    }

    fn insert(
        &mut self,
        expression: String,
        compiled: Result<Arc<T>, Arc<str>>,
    ) -> Result<Arc<T>, Arc<str>> {
        if let Some(existing) = self.values.get(&expression) {
            return existing.clone();
        }
        while self.values.len() >= COMPILE_CACHE_CAPACITY {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.values.remove(&oldest);
            } else {
                break;
            }
        }
        self.insertion_order.push_back(expression.clone());
        self.values.insert(expression, compiled.clone());
        compiled
    }
}

static GATE_CACHE: Lazy<RwLock<CompileCache<GateExpr>>> =
    Lazy::new(|| RwLock::new(CompileCache::new()));
static HINT_DISPLAY_CACHE: Lazy<RwLock<CompileCache<HintDisplayExpr>>> =
    Lazy::new(|| RwLock::new(CompileCache::new()));

fn parse(expr: &str) -> Result<parser::RawSexpr, String> {
    let tokens = parser::tokenize(expr);
    let (sexpr, used) = parser::parse_expr(&tokens).map_err(|e| format!("Parse Error: {e:?}"))?;
    if used != tokens.len() {
        return Err("Parse Error: trailing tokens".to_string());
    }
    Ok(sexpr)
}

fn compile_gate_expr_uncached(expr: &str) -> Result<GateExpr, String> {
    let sexpr = parse(expr)?;
    compiler::compile_gate(&sexpr).map_err(|e| format!("Compile Error: {e:?}"))
}

fn compile_hint_display_expr_uncached(expr: &str) -> Result<HintDisplayExpr, String> {
    let sexpr = parse(expr)?;
    compiler::compile_hint_display(&sexpr).map_err(|e| format!("Compile Error: {e:?}"))
}

fn compile_cached<T>(
    cache: &RwLock<CompileCache<T>>,
    expression: &str,
    compile: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Arc<T>, Arc<str>> {
    if let Some(compiled) = cache.read().unwrap().get(expression) {
        return compiled;
    }
    let compiled = compile(expression).map(Arc::new).map_err(Arc::<str>::from);
    cache
        .write()
        .unwrap()
        .insert(expression.to_string(), compiled)
}

pub fn compile_gate_expr(expr: &str) -> Result<Arc<GateExpr>, Arc<str>> {
    compile_cached(&GATE_CACHE, expr, compile_gate_expr_uncached)
}

pub fn compile_hint_display_expr(expr: &str) -> Result<Arc<HintDisplayExpr>, Arc<str>> {
    compile_cached(
        &HINT_DISPLAY_CACHE,
        expr,
        compile_hint_display_expr_uncached,
    )
}

/// A state-aware S-expression predicate language for gating and progression.
pub fn eval<S: PuzzleStates>(state: &S, expr: &str) -> bool {
    let expr = compile_gate_expr(expr);
    ast::eval_compiled(state, &expr.unwrap())
}

mod test {
    use std::sync::Arc;

    use crate::expr::{
        ast::{GateExpr, HintDisplayExpr},
        compile_gate_expr, compile_hint_display_expr, eval,
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

        fn is_triggered(&self, id: super::types::PuzzleId, key: &str) -> bool {
            id == 2 && key == "extra-content"
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

        assert!(eval(&state, "(triggered 2 extra-content)"));
        assert!(eval(&state, "(triggered alpha extra-content)"));
        assert!(eval(&state, "(true)"));
        assert!(!eval(&state, "(false)"));
        assert!(compile_gate_expr("(true unexpected)").is_err());
        assert!(compile_gate_expr("(false unexpected)").is_err());
        assert!(compile_gate_expr("default").is_err());
        assert!(!eval(&state, "(triggered 2 missing)"));

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
    pub fn test_unknown_slug_failed() {
        let state = TestState {};
        assert!(!eval(&state, "(solved unknown-puzzle)"));
        assert!(!eval(&state, "(all-solved (puzzles intro unknown-puzzle))"));
        assert!(!eval(&state, "(all-solved (round unknown-round))"));
        assert!(!eval(&state, "(ge (solved-count (round unknown-round)) 1)"));
    }

    #[test]
    fn test_hint_display_expr() {
        let state = TestState {};
        let expr =
            compile_hint_display_expr("(and (hint-enabled) (hint-cooled-down) (solved intro))")
                .unwrap();

        assert!(super::ast::eval_hint_display_compiled(
            &state, &expr, true, true
        ));
        assert!(!super::ast::eval_hint_display_compiled(
            &state, &expr, false, true
        ));
        assert!(!super::ast::eval_hint_display_compiled(
            &state, &expr, true, false
        ));
        assert!(super::ast::hint_display_uses_cooldown(&expr));
        let independent = compile_hint_display_expr("(and (hint-enabled) (solved intro))").unwrap();
        assert!(!super::ast::hint_display_uses_cooldown(&independent));
        assert!(compile_gate_expr("(hint-enabled)").is_err());
        assert!(compile_gate_expr("(hint-cooled-down)").is_err());
        assert!(compile_hint_display_expr("(hint-enabled unexpected)").is_err());
    }

    #[test]
    fn compiled_expressions_are_cached_and_simplified() {
        let first = compile_gate_expr("(and (true) (solved intro))").unwrap();
        let second = compile_gate_expr("(and (true) (solved intro))").unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(&*first, GateExpr::Solved(_)));

        let hint = compile_hint_display_expr("(and (true) (or (false) (hint-enabled)))").unwrap();
        assert!(matches!(&*hint, HintDisplayExpr::HintEnabled));

        let first_error = compile_gate_expr("(unknown)").unwrap_err();
        let second_error = compile_gate_expr("(unknown)").unwrap_err();
        assert!(Arc::ptr_eq(&first_error, &second_error));

        assert!(compile_gate_expr("(and (false) (unknown))").is_err());
        assert!(compile_hint_display_expr("(or (true) (unknown))").is_err());
    }

    #[test]
    fn compiled_expression_cache_is_bounded() {
        let mut cache = super::CompileCache::new();
        for index in 0..=super::COMPILE_CACHE_CAPACITY {
            cache.insert(index.to_string(), Ok(Arc::new(index)));
        }
        assert_eq!(cache.values.len(), super::COMPILE_CACHE_CAPACITY);
        assert!(cache.get("0").is_none());
        assert!(
            cache
                .get(&super::COMPILE_CACHE_CAPACITY.to_string())
                .is_some()
        );
    }
}
