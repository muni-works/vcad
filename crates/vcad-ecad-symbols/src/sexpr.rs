//! Low-level S-expression parser for KiCad files.
//!
//! KiCad uses a Lisp-like S-expression format. This module provides `nom`
//! combinators that handle whitespace, quoted strings, atoms, and nested
//! parenthesised lists.

use nom::{
    branch::alt,
    bytes::complete::take_while1,
    character::complete::{char, multispace0},
    combinator::{map, value},
    multi::many0,
    sequence::terminated,
    IResult,
};

/// A KiCad S-expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum SExpr<'a> {
    /// An unquoted atom (keyword, identifier, number).
    Atom(&'a str),
    /// A double-quoted string.
    Str(String),
    /// A parenthesised list of child nodes.
    List(Vec<SExpr<'a>>),
}

impl<'a> SExpr<'a> {
    /// Return the atom text or quoted string contents, or `None` for lists.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SExpr::Atom(s) => Some(s),
            SExpr::Str(s) => Some(s.as_str()),
            SExpr::List(_) => None,
        }
    }

    /// Return the children if this is a list.
    pub fn children(&self) -> Option<&[SExpr<'a>]> {
        match self {
            SExpr::List(v) => Some(v),
            _ => None,
        }
    }

    /// Return the first atom in a list (the "tag" or keyword).
    pub fn tag_name(&self) -> Option<&str> {
        self.children()
            .and_then(|c| c.first())
            .and_then(|n| n.as_str())
    }

    /// Find a child list whose first atom equals `name`.
    pub fn find(&self, name: &str) -> Option<&SExpr<'a>> {
        self.children()?.iter().find(|c| c.tag_name() == Some(name))
    }

    /// Find all child lists whose first atom equals `name`.
    pub fn find_all(&self, name: &str) -> Vec<&SExpr<'a>> {
        match self.children() {
            Some(c) => c.iter().filter(|c| c.tag_name() == Some(name)).collect(),
            None => vec![],
        }
    }

    /// Parse this node as an f64. Works for atoms that are valid numbers.
    pub fn as_f64(&self) -> Option<f64> {
        self.as_str().and_then(|s| s.parse::<f64>().ok())
    }
}

// ---------------------------------------------------------------------------
// nom combinators
// ---------------------------------------------------------------------------

/// Skip whitespace and line comments (lines starting with `#` in some KiCad files,
/// though the main format uses no comments -- we handle it defensively).
fn ws(input: &str) -> IResult<&str, ()> {
    value((), multispace0)(input)
}

/// Parse an unquoted atom: a run of non-whitespace, non-paren characters.
fn atom(input: &str) -> IResult<&str, SExpr<'_>> {
    map(
        take_while1(|c: char| !c.is_whitespace() && c != '(' && c != ')' && c != '"'),
        SExpr::Atom,
    )(input)
}

/// Parse a double-quoted string, handling escaped characters.
fn quoted_string(input: &str) -> IResult<&str, SExpr<'_>> {
    let (input, _) = char('"')(input)?;
    let mut result = String::new();
    let mut remaining = input;
    loop {
        if remaining.is_empty() {
            return Err(nom::Err::Error(nom::error::Error::new(
                remaining,
                nom::error::ErrorKind::Char,
            )));
        }
        let c = remaining.chars().next().unwrap();
        remaining = &remaining[c.len_utf8()..];
        match c {
            '"' => return Ok((remaining, SExpr::Str(result))),
            '\\' => {
                if let Some(escaped) = remaining.chars().next() {
                    remaining = &remaining[escaped.len_utf8()..];
                    match escaped {
                        'n' => result.push('\n'),
                        't' => result.push('\t'),
                        '\\' => result.push('\\'),
                        '"' => result.push('"'),
                        other => {
                            result.push('\\');
                            result.push(other);
                        }
                    }
                }
            }
            other => result.push(other),
        }
    }
}

/// Parse a parenthesised list: `( ... )`.
fn list(input: &str) -> IResult<&str, SExpr<'_>> {
    let (input, _) = char('(')(input)?;
    let (input, _) = ws(input)?;
    let (input, children) = many0(terminated(sexpr_node, ws))(input)?;
    let (input, _) = char(')')(input)?;
    Ok((input, SExpr::List(children)))
}

/// Parse a single S-expression node: atom, quoted string, or list.
fn sexpr_node(input: &str) -> IResult<&str, SExpr<'_>> {
    alt((list, quoted_string, atom))(input)
}

/// Deepest list nesting accepted. Real KiCad files nest < 30 deep; the
/// parser recurses per level, so hostile nesting would abort the process
/// with a stack overflow (fuzz F8).
pub const MAX_SEXPR_DEPTH: usize = 512;

/// Whether `input`'s parenthesis nesting (outside quoted strings) goes
/// deeper than `max`.
fn depth_exceeds(input: &str, max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for b in input.bytes() {
        if in_str {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// Parse the entire input as a top-level S-expression.
pub fn parse_sexpr(input: &str) -> IResult<&str, SExpr<'_>> {
    if depth_exceeds(input, MAX_SEXPR_DEPTH) {
        return Err(nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::TooLarge,
        )));
    }
    let (input, _) = ws(input)?;
    let (input, node) = sexpr_node(input)?;
    let (input, _) = ws(input)?;
    Ok((input, node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_atom() {
        let (rest, node) = sexpr_node("hello world").unwrap();
        assert_eq!(node.as_str(), Some("hello"));
        assert_eq!(rest, " world");
    }

    #[test]
    fn parse_quoted() {
        let (rest, node) = sexpr_node(r#""hello world" rest"#).unwrap();
        assert_eq!(node.as_str(), Some("hello world"));
        assert_eq!(rest, " rest");
    }

    #[test]
    fn parse_escaped_quotes() {
        let (_rest, node) = sexpr_node(r#""he said \"hi\"" rest"#).unwrap();
        assert_eq!(node.as_str(), Some(r#"he said "hi""#));
    }

    #[test]
    fn parse_simple_list() {
        let (_rest, node) = sexpr_node("(foo bar 42)").unwrap();
        assert_eq!(node.tag_name(), Some("foo"));
        let children = node.children().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(children[1].as_str(), Some("bar"));
        assert_eq!(children[2].as_str(), Some("42"));
    }

    #[test]
    fn parse_nested_list() {
        let (_, node) = sexpr_node("(a (b c) (d (e f)))").unwrap();
        assert_eq!(node.tag_name(), Some("a"));
        let children = node.children().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(children[1].tag_name(), Some("b"));
        assert_eq!(children[2].tag_name(), Some("d"));
    }

    #[test]
    fn find_child() {
        let (_, node) = sexpr_node("(root (name \"test\") (value 42))").unwrap();
        let name_node = node.find("name").unwrap();
        let children = name_node.children().unwrap();
        assert_eq!(children[1].as_str(), Some("test"));
    }

    #[test]
    fn find_all_children() {
        let (_, node) = sexpr_node("(root (pin 1) (pin 2) (pin 3))").unwrap();
        let pins = node.find_all("pin");
        assert_eq!(pins.len(), 3);
    }
}

#[cfg(test)]
mod fuzz_regressions {
    /// Fuzz finding F8 (docs/qa/fuzz-2026-11-12.md): deeply nested lists
    /// overflowed the stack in every KiCad reader. Must be a clean error,
    /// even on a small worker-thread stack.
    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let depth = 200_000;
        let text = format!("(kicad_pcb {}{})", "(a ".repeat(depth), ")".repeat(depth));
        let t2 = text.replace("kicad_pcb", "kicad_sch");
        let pcb = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || crate::parse_kicad_pcb(&text).is_err())
            .unwrap()
            .join();
        assert_eq!(pcb.ok(), Some(true));
        let sch = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || crate::parse_kicad_sch(&t2).is_err())
            .unwrap()
            .join();
        assert_eq!(sch.ok(), Some(true));
        // A quoted "(" doesn't count toward depth.
        assert!(!super::depth_exceeds(r#"(a "((((((((((")"#, 3));
    }
}
