//! Variadic `-` / `/`, and the library's names staying its own when a user
//! binding shadows a builtin or library global.

use std::collections::HashMap;

use loon_lang::interp::Value;
use vcad_loon::{eval_vcad, eval_vcad_to_value, eval_vcad_with_modules};

fn num(src: &str) -> f64 {
    match eval_vcad_to_value(src, None).unwrap_or_else(|e| panic!("{src}: {e}")) {
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
        other => panic!("{src}: expected a number, got {other}"),
    }
}

#[test]
fn minus_folds_left_over_every_argument() {
    // The reported case: the third operand used to be dropped (-8).
    assert_eq!(num("[let r16 8]\n[- 0 r16 1]"), -9.0);
    assert_eq!(num("[- 10 1 2 3]"), 4.0);
    assert_eq!(num("[- 10.0 0.5 0.25]"), 9.25);
    assert_eq!(num("[- 7 2]"), 5.0);
}

#[test]
fn unary_minus_negates() {
    assert_eq!(num("[- 5]"), -5.0);
    assert_eq!(num("[- 2.5]"), -2.5);
    assert!(matches!(
        eval_vcad_to_value("[- 5]", None),
        Ok(Value::Int(-5))
    ));
}

#[test]
fn divide_folds_left() {
    assert_eq!(num("[/ 24 2 3]"), 4.0);
    assert_eq!(num("[/ 1.0 2.0 4.0]"), 0.125);
    assert_eq!(num("[/ 9 3]"), 3.0);
    assert!(eval_vcad_to_value("[/ 4]", None).is_err());
}

#[test]
fn plus_and_times_were_already_variadic() {
    assert_eq!(num("[+ 1 2 3 4]"), 10.0);
    assert_eq!(num("[* 2 3 4]"), 24.0);
}

#[test]
fn minus_still_works_as_a_value() {
    assert_eq!(num("[fold #[1 2 3] 10 -]"), 4.0);
    assert_eq!(num("[let sub -]\n[sub 3 1 1]"), 1.0);
}

#[test]
fn arithmetic_errors_still_error() {
    assert!(eval_vcad_to_value("[- \"a\" 1]", None).is_err());
    assert!(eval_vcad_to_value("[- 1 \"a\" 2]", None).is_err());
    assert!(eval_vcad_to_value("[/ 1 0]", None).is_err());
}

#[test]
fn user_len_does_not_break_the_library() {
    // `union` calls `len` internally. This used to fail "not callable: 66".
    let doc = eval_vcad(
        "[let a 30]\n[let b 36]\n[let len [+ a b]]\n\
         [root [union [cube len 1 1] [cube 1 1 1] [cube 2 2 2]] \"steel\"]",
        None,
    )
    .unwrap();
    assert_eq!(doc.roots.len(), 1);
    // And the user's own `len` is what the user bound.
    assert_eq!(
        num("[let len [+ 30 36]]\n[let u [union [cube 1 1 1] [cube 2 2 2]]]\nlen"),
        66.0
    );
}

#[test]
fn user_function_named_like_a_builtin_is_not_called_by_the_library() {
    // A user `fold` that returns garbage would silently change every
    // boolean if the library still looked it up dynamically.
    let doc = eval_vcad(
        "[let fold [fn [& _] 0]]\n[let take [fn [& _] 0]]\n\
         [root [difference [cube 1 1 1] [cube 3 3 3]] \"steel\"]",
        None,
    )
    .unwrap();
    assert_eq!(doc.roots.len(), 1);
}

#[test]
fn user_shadowing_a_library_global_keeps_the_library_intact() {
    // `fold-subject-last` is a library helper `union` depends on.
    let doc = eval_vcad(
        "[let fold-subject-last 3]\n[let union-r [union [cube 1 1 1] [cube 2 2 2]]]\n\
         [root union-r \"steel\"]",
        None,
    )
    .unwrap();
    assert_eq!(doc.roots.len(), 1);
}

#[test]
fn user_can_still_use_their_shadowing_binding() {
    assert_eq!(num("[let len [fn [x] 42]]\n[len #[1 2 3]]"), 42.0);
    assert_eq!(num("[let - 3]\n-"), 3.0);
}

#[test]
fn shadowing_inside_a_module_keeps_the_library_intact() {
    let mut m = HashMap::new();
    m.insert(
        "m".to_string(),
        "[let len 66]\n[pub let thing [union [cube 1 1 1] [cube 2 2 2]]]\n\
         [pub let neg [- 0 8 1]]"
            .to_string(),
    );
    let doc = eval_vcad_with_modules("[use m]\n[root m.thing \"steel\"]", None, &m).unwrap();
    assert_eq!(doc.roots.len(), 1);
    let v = vcad_loon::eval_vcad_to_value_with_modules("[use m]\nm.neg", None, &m).unwrap();
    assert!(matches!(v, Value::Int(-9)), "{v}");
}

#[test]
fn arithmetic_error_still_points_at_the_user_line() {
    let dir = std::env::temp_dir().join(format!("vcad-loon-arith-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bad.loon");
    std::fs::write(
        &p,
        "[let w 10]\n\n[root [cube [- w \"x\" 1] 1 1] \"steel\"]\n",
    )
    .unwrap();
    let d = vcad_loon::eval_vcad_file_diagnostic(&p).unwrap_err();
    assert_eq!(d.line, Some(3), "{d}");
}
