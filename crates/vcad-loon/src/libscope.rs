//! Lexical capture for the bundled vcad library.
//!
//! The library and the user's program run as one loon program over one
//! global map, and loon resolves globals when a call happens, not when a
//! function is defined. So a user's `[let len [+ a b]]` used to replace the
//! `len` that `union` calls internally, and the next `union` died with
//! "not callable: 66". Worse, a user function bound to a library name would
//! be called silently.
//!
//! This pass gives the library its own names. Every free reference in a
//! library function body to a global (an interpreter builtin or a library
//! top-level binding) is rewritten to a private alias, `%lib%<name>`. The
//! aliases are bound once, as the library loads: builtins at the very top,
//! and each library global right after the form that defines it. User code
//! is never touched, and it can still shadow `len` or `union` for its own
//! use. The library just no longer sees that.
//!
//! The rewrite works on the library *source text*, splicing at symbol
//! spans, so the same rewritten text serves the root program and the
//! prelude that every `[use]`d module evaluates.

use std::collections::HashSet;
use std::sync::OnceLock;

use loon_lang::ast::{Expr, ExprKind};

/// Prefix of the private alias for a library-referenced global. `%` is a
/// legal symbol start, and nobody writes `%lib%len` by accident.
pub(crate) const ALIAS_PREFIX: &str = "%lib%";

/// The library source with its references captured. Falls back to the raw
/// source if it does not parse (it always does; a test pins that).
pub(crate) fn captured_lib(raw: &'static str) -> &'static str {
    static CAPTURED: OnceLock<String> = OnceLock::new();
    CAPTURED.get_or_init(|| capture(raw).unwrap_or_else(|| raw.to_string()))
}

fn alias(name: &str) -> String {
    format!("{ALIAS_PREFIX}{name}")
}

/// Heads the interpreter dispatches on before looking anything up. Never
/// rewritten, even if a builtin of the same name exists: `[%lib%if …]`
/// would turn a special form into a call.
const SPECIAL_FORMS: &[&str] = &[
    "catch-errors", "derive", "do", "effect", "fn", "handle", "if", "impl", "inspect", "let",
    "loop", "macro", "macro+", "macroexpand", "match", "mut", "pipe", "pub", "recur", "set!",
    "sig", "test", "trait", "try", "type", "use", "when",
];

/// Names bound in the interpreter's global scope before any source runs.
fn builtin_names() -> HashSet<String> {
    let mut env = loon_lang::interp::Env::new();
    loon_lang::interp::register_builtins_pub(&mut env);
    env.globals()
        .into_keys()
        .filter(|n| !SPECIAL_FORMS.contains(&n.as_str()))
        .collect()
}

/// The name a top-level library form binds globally, if any:
/// `[let name …]`, `[let mut name …]`, `[fn name …]`.
fn top_level_binding(e: &Expr) -> Option<&str> {
    let ExprKind::List(items) = &e.kind else {
        return None;
    };
    let head = sym(items.first()?)?;
    match head {
        "let" => {
            let mut i = 1;
            if items.get(i).and_then(sym) == Some("mut") {
                i += 1;
            }
            // Statement form only; `[let x v body]` binds nothing global.
            if items.len() != i + 2 {
                return None;
            }
            sym(items.get(i)?)
        }
        "fn" => sym(items.get(1)?),
        _ => None,
    }
}

fn sym(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Symbol(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Rewrite `raw`, or `None` if it does not parse.
pub(crate) fn capture(raw: &str) -> Option<String> {
    let exprs = loon_lang::parser::parse(raw).ok()?;
    let builtins = builtin_names();
    let lib_globals: Vec<(&str, usize)> = exprs
        .iter()
        .filter_map(|e| top_level_binding(e).map(|n| (n, e.span.end)))
        .collect();
    let mut protected: HashSet<String> = builtins.clone();
    protected.extend(lib_globals.iter().map(|(n, _)| n.to_string()));
    protected.retain(|n| !SPECIAL_FORMS.contains(&n.as_str()));

    let mut w = Walker {
        src: raw,
        protected: &protected,
        scopes: Vec::new(),
        edits: Vec::new(),
        referenced: HashSet::new(),
    };
    for e in &exprs {
        w.top_level(e);
    }
    let Walker {
        mut edits,
        referenced,
        ..
    } = w;

    // Builtins the library references get their alias before anything else
    // runs, including any the library later redefines (`-`, `/`) so a
    // top-level use ahead of the redefinition still sees the builtin.
    let mut head: Vec<&String> = referenced.iter().filter(|n| builtins.contains(*n)).collect();
    head.sort();
    let mut prologue = String::new();
    for n in head {
        prologue.push_str(&format!("[let {} {n}]\n", alias(n)));
    }
    // Library globals: re-point the alias right after each definition.
    for (name, end) in &lib_globals {
        if referenced.contains(*name) {
            edits.push((*end..*end, format!(" [let {} {name}]", alias(name))));
        }
    }

    edits.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = prologue;
    let mut at = 0;
    for (range, text) in edits {
        if range.start < at {
            return None; // overlapping edits: should be impossible
        }
        out.push_str(&raw[at..range.start]);
        out.push_str(&text);
        at = range.end;
    }
    out.push_str(&raw[at..]);
    Some(out)
}

struct Walker<'a> {
    src: &'a str,
    protected: &'a HashSet<String>,
    /// Local scopes, innermost last. Empty at top level.
    scopes: Vec<HashSet<String>>,
    edits: Vec<(std::ops::Range<usize>, String)>,
    referenced: HashSet<String>,
}

impl Walker<'_> {
    fn bound(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn top_level(&mut self, e: &Expr) {
        self.visit(e);
    }

    fn reference(&mut self, e: &Expr, name: &str) {
        if self.bound(name) || !self.protected.contains(name) {
            return;
        }
        // Only splice where the source text *is* the symbol; desugared
        // nodes (string interpolation, dotted access) keep dynamic lookup.
        let span = e.span.start..e.span.end;
        if self.src.get(span.clone()) != Some(name) {
            return;
        }
        self.referenced.insert(name.to_string());
        self.edits.push((span, alias(name)));
    }

    fn with_scope(&mut self, names: HashSet<String>, f: impl FnOnce(&mut Self)) {
        self.scopes.push(names);
        f(self);
        self.scopes.pop();
    }

    fn visit_all(&mut self, es: &[Expr]) {
        for e in es {
            self.visit(e);
        }
    }

    fn visit(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Symbol(s) => self.reference(e, s),
            ExprKind::List(items) => self.visit_list(items),
            ExprKind::Vec(xs) | ExprKind::Set(xs) | ExprKind::Tuple(xs) => self.visit_all(xs),
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.visit(k);
                    self.visit(v);
                }
            }
            ExprKind::DotAccess(inner, _) => self.visit(inner),
            // Quoted templates are data until spliced; leave them alone.
            ExprKind::Quote(_) | ExprKind::Unquote(_) | ExprKind::UnquoteSplice(_) => {}
            _ => {}
        }
    }

    fn visit_list(&mut self, items: &[Expr]) {
        match items.first().and_then(sym) {
            Some("fn") => self.visit_fn(&items[1..]),
            Some("let") => self.visit_let(&items[1..]),
            Some("match") => self.visit_match(&items[1..]),
            // Type definitions name constructors and field types, not values.
            Some("type") => {}
            // The target is a binding, not a reference.
            Some("set!") => self.visit_all(items.get(2..).unwrap_or(&[])),
            _ => self.visit_all(items),
        }
    }

    /// `[fn name? [params] #{effects}? body…]` or `[fn name? (params body…)…]`.
    fn visit_fn(&mut self, args: &[Expr]) {
        let args = match args.first().map(|a| &a.kind) {
            Some(ExprKind::Symbol(_)) => &args[1..],
            _ => args,
        };
        if matches!(args.first().map(|a| &a.kind), Some(ExprKind::Tuple(_))) {
            for clause in args {
                if let ExprKind::Tuple(c) = &clause.kind {
                    if let Some((params, body)) = c.split_first() {
                        self.with_scope(binders(params), |w| w.visit_all(body));
                    }
                }
            }
            return;
        }
        let Some((params, mut body)) = args.split_first() else {
            return;
        };
        if matches!(body.first().map(|b| &b.kind), Some(ExprKind::Set(_))) {
            body = &body[1..];
        }
        self.with_scope(binders(params), |w| w.visit_all(body));
    }

    /// `[let mut? binding value body…]`.
    fn visit_let(&mut self, args: &[Expr]) {
        let args = if args.first().and_then(sym) == Some("mut") {
            &args[1..]
        } else {
            args
        };
        let [binding, value, body @ ..] = args else {
            self.visit_all(args);
            return;
        };
        self.visit(value);
        let names = binders(binding);
        if !body.is_empty() {
            self.with_scope(names, |w| w.visit_all(body));
        } else if let Some(scope) = self.scopes.last_mut() {
            // Statement form inside a body binds in the current local scope.
            scope.extend(names);
        }
        // Statement form at top level is a library global: already protected.
    }

    /// `[match scrutinee pat body pat [when guard] body …]`.
    fn visit_match(&mut self, args: &[Expr]) {
        let Some((scrutinee, arms)) = args.split_first() else {
            return;
        };
        self.visit(scrutinee);
        let mut i = 0;
        while i + 1 < arms.len() {
            let names = binders(&arms[i]);
            let guard = arms
                .get(i + 1)
                .filter(|g| i + 2 < arms.len() && is_when(g));
            if let Some(g) = guard {
                let body = &arms[i + 2];
                self.with_scope(names, |w| {
                    w.visit(g);
                    w.visit(body);
                });
                i += 3;
            } else {
                let body = &arms[i + 1];
                self.with_scope(names, |w| w.visit(body));
                i += 2;
            }
        }
    }
}

fn is_when(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::List(xs) if xs.first().and_then(sym) == Some("when"))
}

/// Every name a parameter list, `let` binding, or match pattern can bind.
/// Over-collecting is safe: a name wrongly treated as local just keeps the
/// old dynamic lookup.
fn binders(e: &Expr) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_binders(e, &mut out);
    out
}

fn collect_binders(e: &Expr, out: &mut HashSet<String>) {
    match &e.kind {
        ExprKind::Symbol(s) => {
            let constructor = s.starts_with(|c: char| c.is_ascii_uppercase());
            if s != "_" && s != "&" && !constructor {
                out.insert(s.clone());
            }
        }
        // `[Ctor a b]` in a pattern: the head names the variant.
        ExprKind::List(xs) => {
            let rest = match xs.first().map(|x| &x.kind) {
                Some(ExprKind::Symbol(s)) if s.starts_with(|c: char| c.is_ascii_uppercase()) => {
                    &xs[1..]
                }
                _ => &xs[..],
            };
            for x in rest {
                collect_binders(x, out);
            }
        }
        ExprKind::Vec(xs) | ExprKind::Tuple(xs) | ExprKind::Set(xs) => {
            for x in xs {
                collect_binders(x, out);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                collect_binders(k, out);
                collect_binders(v, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lib_captures_and_still_parses() {
        let raw = crate::VCAD_LIB_SOURCE;
        let out = capture(raw).expect("lib parses");
        assert!(loon_lang::parser::parse(&out).is_ok());
        // The reference that bit: `union` → fold-subject-last → `[len args]`.
        assert!(out.contains("[%lib%len args]"), "len not captured");
        assert!(out.contains("[let %lib%len len]"));
        assert!(out.contains("[let %lib%fold-subject-last fold-subject-last]"));
    }

    #[test]
    fn locals_are_not_captured() {
        let src = "[let f [fn [len] [len 1]]]\n[let g [fn [x] [let len 3 [+ len x]]]]\n[let h [fn [x] [len x]]]";
        let out = capture(src).unwrap();
        assert!(out.contains("[fn [len] [len 1]]"), "{out}");
        assert!(out.contains("[let len 3 [%lib%+ len x]]"), "{out}");
        assert!(out.contains("[fn [x] [%lib%len x]]"), "{out}");
    }

    #[test]
    fn match_bindings_are_not_captured() {
        let src = "[let f [fn [v] [match v [Some len] [len 1] _ [len v]]]]";
        let out = capture(src).unwrap();
        assert!(out.contains("[Some len] [len 1] _ [%lib%len v]"), "{out}");
    }
}
