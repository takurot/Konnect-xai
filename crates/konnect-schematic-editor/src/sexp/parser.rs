use super::SexpNode;
use crate::error::{Error, Result};
use konnect_sexp::parser::MAX_LIST_DEPTH;

pub fn parse(input: &str) -> Result<SexpNode> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    parse_node(bytes, &mut pos, 0)
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_node(b: &[u8], pos: &mut usize, depth: usize) -> Result<SexpNode> {
    skip_ws(b, pos);
    if *pos >= b.len() {
        return Err(Error::Parse {
            pos: *pos,
            msg: "Unexpected end of input".into(),
        });
    }
    match b[*pos] {
        b'(' => parse_list(b, pos, depth),
        b'"' => parse_string(b, pos),
        _ => parse_atom(b, pos),
    }
}

fn parse_list(b: &[u8], pos: &mut usize, depth: usize) -> Result<SexpNode> {
    if depth >= MAX_LIST_DEPTH {
        return Err(Error::Parse {
            pos: *pos,
            msg: format!("S-expression nesting exceeds the limit of {MAX_LIST_DEPTH} lists"),
        });
    }
    *pos += 1; // consume '('
    let mut children = Vec::new();
    loop {
        skip_ws(b, pos);
        if *pos >= b.len() {
            return Err(Error::Parse {
                pos: *pos,
                msg: "Unclosed parenthesis".into(),
            });
        }
        if b[*pos] == b')' {
            *pos += 1;
            break;
        }
        children.push(parse_node(b, pos, depth + 1)?);
    }
    Ok(SexpNode::List(children))
}

fn parse_string(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    *pos += 1; // consume '"'
    let mut s: Vec<u8> = Vec::new();
    loop {
        if *pos >= b.len() {
            return Err(Error::Parse {
                pos: *pos,
                msg: "Unterminated string".into(),
            });
        }
        match b[*pos] {
            b'"' => {
                *pos += 1;
                break;
            }
            b'\\' => {
                *pos += 1;
                if *pos >= b.len() {
                    return Err(Error::Parse {
                        pos: *pos,
                        msg: "Unterminated escape".into(),
                    });
                }
                match b[*pos] {
                    b'"' => s.push(b'"'),
                    b'\\' => s.push(b'\\'),
                    b'n' => s.push(b'\n'),
                    b't' => s.push(b'\t'),
                    b'r' => s.push(b'\r'),
                    c => {
                        s.push(b'\\');
                        s.push(c);
                    }
                }
                *pos += 1;
            }
            c => {
                s.push(c);
                *pos += 1;
            }
        }
    }
    Ok(SexpNode::Str(String::from_utf8_lossy(&s).into_owned()))
}

fn parse_atom(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    let start = *pos;
    while *pos < b.len() && !matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'"') {
        *pos += 1;
    }
    if *pos == start {
        return Err(Error::Parse {
            pos: *pos,
            msg: format!("Unexpected character: '{}'", b[*pos] as char),
        });
    }
    Ok(SexpNode::Atom(
        String::from_utf8_lossy(&b[start..*pos]).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_limit_accepts_deepest_list_and_rejects_next_opening() {
        let prefix = "(label \"日本語\" \n  ";
        let allowed = format!("{prefix}{}atom{}", "(".repeat(255), ")".repeat(256));
        assert!(parse(&allowed).is_ok());
        let too_deep = format!("{prefix}{}atom{}", "(".repeat(256), ")".repeat(257));
        let error = parse(&too_deep).unwrap_err();
        assert!(matches!(error, Error::Parse { pos, ref msg }
            if pos == prefix.len() + 255 && msg.contains("256")));
    }

    #[test]
    fn nesting_limit_ignores_parentheses_in_quoted_values() {
        let value = "(())".repeat(257);
        let input = format!("(label \"{value}\")");
        let parsed = parse(&input).unwrap();
        assert_eq!(parsed.value(), Some(value.as_str()));
    }
}
