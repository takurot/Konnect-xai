//! nom-based S-expression parser for KiCAD files.
//!
//! Produces a lightweight `SexpNode` tree used for **reading** data only.
//! All writes are done as targeted text edits (see `writer.rs`).

use crate::SexpError;
use nom::{
    branch::alt,
    bytes::complete::{escaped, take_while1},
    character::complete::{char, multispace0, none_of},
    combinator::{map, recognize},
    error::{Error, ErrorKind},
    multi::many0,
    sequence::preceded,
    IResult, Parser,
};

// ─── AST ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SexpNode {
    /// A bare atom (identifier, number, uuid, etc.)
    Atom(String),
    /// A double-quoted string
    Str(String),
    /// A list: `(head child child ...)`
    List(Vec<SexpNode>),
}

impl SexpNode {
    /// Return the string value if this is an Atom or Str.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SexpNode::Atom(s) | SexpNode::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Return the children if this is a List.
    pub fn children(&self) -> Option<&[SexpNode]> {
        match self {
            SexpNode::List(v) => Some(v),
            _ => None,
        }
    }

    /// Return the head (first child) of a List, if any.
    pub fn head(&self) -> Option<&str> {
        self.children()?.first()?.as_str()
    }

    /// Find the first direct child List whose head matches `tag`.
    pub fn find(&self, tag: &str) -> Option<&SexpNode> {
        self.children()?.iter().find(|n| n.head() == Some(tag))
    }

    /// Collect all direct child Lists whose head matches `tag`.
    pub fn find_all(&self, tag: &str) -> Vec<&SexpNode> {
        self.children()
            .unwrap_or(&[])
            .iter()
            .filter(|n| n.head() == Some(tag))
            .collect()
    }

    /// Get the n-th child (0-indexed).
    pub fn get(&self, n: usize) -> Option<&SexpNode> {
        self.children()?.get(n)
    }

    /// Parse the n-th child as f64.
    pub fn get_f64(&self, n: usize) -> Option<f64> {
        self.get(n)?.as_str()?.parse().ok()
    }

    /// Convenience: find a child by tag and return its 1st data child as str.
    pub fn find_str(&self, tag: &str) -> Option<&str> {
        self.find(tag)?.get(1)?.as_str()
    }

    /// Convenience: find a child by tag and return its 1st data child as f64.
    pub fn find_f64(&self, tag: &str) -> Option<f64> {
        self.find(tag)?.get(1)?.as_str()?.parse().ok()
    }
}

// ─── Parser ───────────────────────────────────────────────────────────────────

/// Maximum number of nested lists in a parsed KiCad expression, including its root list.
pub const MAX_LIST_DEPTH: usize = 256;

/// Parse a full KiCAD S-expression document into a `SexpNode::List`.
pub fn parse_sexp(input: &str) -> Result<SexpNode, SexpError> {
    let (rest, node) = sexp(input, 0).map_err(|error| parse_error(input, error))?;
    let mut remaining = rest.trim_start();
    if remaining.is_empty() {
        return Ok(node);
    }

    // Multiple top-level nodes are wrapped in an implicit list.
    let mut nodes = vec![node];
    while !remaining.is_empty() {
        match sexp(remaining, 0) {
            Ok((rest, node)) => {
                nodes.push(node);
                remaining = rest.trim_start();
            }
            // A nesting limit error must not be swallowed as a trailing fragment.
            Err(error @ nom::Err::Failure(_)) => return Err(parse_error(input, error)),
            Err(_) => break,
        }
    }
    Ok(SexpNode::List(nodes))
}

fn parse_error(input: &str, error: nom::Err<Error<&str>>) -> SexpError {
    match error {
        nom::Err::Failure(Error {
            input: remaining,
            code: ErrorKind::TooLarge,
        }) => SexpError::Parse {
            offset: input.len() - remaining.len(),
            message: format!("S-expression nesting exceeds the limit of {MAX_LIST_DEPTH} lists"),
        },
        error => SexpError::Parse {
            offset: 0,
            message: format!("{error}"),
        },
    }
}

fn sexp(input: &str, depth: usize) -> IResult<&str, SexpNode> {
    preceded(
        multispace0,
        alt((
            |input| parse_list(input, depth),
            parse_quoted_string,
            parse_atom,
        )),
    )
    .parse(input)
}

fn parse_list(input: &str, depth: usize) -> IResult<&str, SexpNode> {
    let (remaining, _) = char('(').parse(input)?;
    if depth >= MAX_LIST_DEPTH {
        // Failure prevents nom from treating an over-deep list as the end of
        // the parent's children or backtracking to a scalar parser.
        return Err(nom::Err::Failure(Error::new(input, ErrorKind::TooLarge)));
    }
    let (remaining, children) = many0(|input| sexp(input, depth + 1)).parse(remaining)?;
    let (remaining, _) = preceded(multispace0, char(')')).parse(remaining)?;
    Ok((remaining, SexpNode::List(children)))
}

fn parse_quoted_string(input: &str) -> IResult<&str, SexpNode> {
    // Handle empty strings "" as a special case, then fall back to escaped content.
    let (input, _) = char('"').parse(input)?;
    if let Ok((rest, _)) = char::<&str, nom::error::Error<&str>>('"').parse(input) {
        return Ok((rest, SexpNode::Str(String::new())));
    }
    let (input, content) = recognize(escaped(
        none_of("\\\""),
        '\\',
        nom::character::complete::anychar,
    ))
    .parse(input)?;
    let (input, _) = char('"').parse(input)?;
    Ok((input, SexpNode::Str(unescape(content))))
}

fn parse_atom(input: &str) -> IResult<&str, SexpNode> {
    map(
        take_while1(|c: char| !c.is_whitespace() && c != '(' && c != ')' && c != '"'),
        |s: &str| SexpNode::Atom(s.to_string()),
    )
    .parse(input)
}

/// Decode the escape sequences KiCad writes inside quoted strings.
///
/// Must be a single left-to-right pass. Chained `replace` calls collapse
/// backslashes *last*, so an already-escaped backslash gets re-read as the
/// introducer of the following escape: `C:\\new` (a literal `C:\new`) came
/// back as `C:\` + a newline, and `a\\tb` as `a\` + a tab. KiCad escapes
/// backslashes in property values, so any Windows path whose next character
/// was `n` or `t` decoded wrongly.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            // Unknown escape: keep both characters rather than guessing, so
            // the value round-trips instead of silently losing a backslash.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_limit_accepts_deepest_list_and_rejects_next_opening() {
        let prefix = " \n";
        let allowed = format!("{prefix}{}atom{}", "(".repeat(256), ")".repeat(256));
        assert!(parse_sexp(&allowed).is_ok());
        let too_deep = format!("{prefix}{}atom{}", "(".repeat(257), ")".repeat(257));
        let error = parse_sexp(&too_deep).unwrap_err();
        assert!(matches!(error, SexpError::Parse { offset, ref message }
            if offset == prefix.len() + 256 && message.contains("256")));
    }

    #[test]
    fn nesting_limit_carries_byte_offset_across_top_level_forms() {
        let prefix = "(label \"日本語\")  \n  ";
        let too_deep = format!("{prefix}{}atom{} \n  ", "(".repeat(257), ")".repeat(257));
        let error = parse_sexp(&too_deep).unwrap_err();
        assert!(matches!(error, SexpError::Parse { offset, ref message }
            if offset == prefix.len() + 256 && message.contains("256")));
    }

    #[test]
    fn nesting_limit_ignores_parentheses_in_quoted_values() {
        let value = "(())".repeat(257);
        let input = format!("(label \"{value}\")");
        let parsed = parse_sexp(&input).unwrap();
        assert_eq!(
            parsed.get(1).and_then(SexpNode::as_str),
            Some(value.as_str())
        );
    }

    #[test]
    fn atom() {
        let n = parse_sexp("hello").unwrap();
        assert_eq!(n.as_str(), Some("hello"));
    }

    #[test]
    fn quoted_string() {
        let n = parse_sexp(r#""hello world""#).unwrap();
        assert_eq!(n.as_str(), Some("hello world"));
    }

    #[test]
    fn simple_list() {
        let n = parse_sexp("(wire (start 1.0 2.0) (end 3.0 4.0))").unwrap();
        assert_eq!(n.head(), Some("wire"));
        let start = n.find("start").unwrap();
        assert_eq!(start.get_f64(1), Some(1.0));
        assert_eq!(start.get_f64(2), Some(2.0));
    }

    #[test]
    fn nested() {
        let s = r#"(kicad_sch (version 20231120) (generator "eeschema"))"#;
        let n = parse_sexp(s).unwrap();
        assert_eq!(n.head(), Some("kicad_sch"));
        assert_eq!(n.find_f64("version"), Some(20231120.0));
        assert_eq!(n.find_str("generator"), Some("eeschema"));
    }

    #[test]
    fn quoted_uuid() {
        // KiCAD 9+ requires quoted UUIDs
        let s = r#"(uuid "abc-123-def")"#;
        let n = parse_sexp(s).unwrap();
        assert_eq!(n.find_str("uuid"), None); // uuid is the head
        assert_eq!(n.get(1).and_then(|n| n.as_str()), Some("abc-123-def"));
    }

    /// Chained `replace` collapsed backslashes last, so an escaped backslash
    /// followed by `n` or `t` was re-read as a control escape. KiCad escapes
    /// backslashes in property values, so Windows paths decoded wrongly.
    #[test]
    fn escaped_backslash_is_not_reread_as_an_escape() {
        // Serialized `C:\\new\\temp` is the literal path `C:\new\temp`.
        let n = parse_sexp(r#"(p "C:\\new\\temp")"#).unwrap();
        assert_eq!(n.get(1).and_then(|c| c.as_str()), Some(r"C:\new\temp"));

        let n = parse_sexp(r#"(p "a\\tb")"#).unwrap();
        assert_eq!(n.get(1).and_then(|c| c.as_str()), Some(r"a\tb"));
    }

    /// Genuine control escapes must still decode.
    #[test]
    fn control_escapes_still_decode() {
        let n = parse_sexp(r#"(p "line1\nline2\tend")"#).unwrap();
        assert_eq!(n.get(1).and_then(|c| c.as_str()), Some("line1\nline2\tend"));
    }

    /// An escaped quote inside a string stays a quote.
    #[test]
    fn escaped_quote_decodes() {
        let n = parse_sexp(r#"(p "say \"hi\"")"#).unwrap();
        assert_eq!(n.get(1).and_then(|c| c.as_str()), Some(r#"say "hi""#));
    }
}
