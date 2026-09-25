//! Line-based lexer: indentation becomes INDENT/DEDENT, and each line is cut
//! into tokens by trying the profile's rules in order (the first match wins).
//! It reproduces Hana's regex lexer exactly, without regexes.

use crate::profile::{Class, Profile, Rule};
use crate::token::{Kind, Token};

pub fn tokenize<'a>(input: &'a str, profile: &Profile) -> Vec<Token<'a>> {
    let mut tokens = Vec::new();
    let mut indents = vec![0usize];
    let mut line_num: u32 = 1;

    for mut line in input.split('\n') {
        for marker in profile.comment_markers {
            if let Some(i) = line.find(marker) {
                line = &line[..i];
            }
        }
        if line.trim().is_empty() {
            line_num += 1;
            continue;
        }

        let indent = line.bytes().take_while(|&b| b == b' ' || b == b'\t').count();
        if indent > *indents.last().unwrap() {
            indents.push(indent);
            tokens.push(Token { kind: Kind::Indent, lit: "", line: line_num, col: 0 });
        } else {
            while indent < *indents.last().unwrap() {
                indents.pop();
                tokens.push(Token { kind: Kind::Dedent, lit: "", line: line_num, col: 0 });
            }
        }

        let mut rest = line.trim();
        let mut col = indent as u32;
        while !rest.is_empty() {
            let mut matched = false;
            for &(kind, rule) in profile.rules {
                if let Some(len) = match_rule(rule, rest) {
                    let lit = &rest[..len];
                    if !matches!(rule, Rule::Space(_)) {
                        tokens.push(Token { kind, lit, line: line_num, col });
                    }
                    rest = &rest[len..];
                    col += lit.chars().count() as u32;
                    matched = true;
                    break;
                }
            }
            if !matched {
                // No rule accepts this character: keep it so the parser reports it.
                let len = rest.chars().next().unwrap().len_utf8();
                tokens.push(Token { kind: Kind::Illegal, lit: &rest[..len], line: line_num, col });
                rest = &rest[len..];
                col += 1;
            }
        }
        line_num += 1;
    }

    for _ in 1..indents.len() {
        tokens.push(Token { kind: Kind::Dedent, lit: "", line: line_num, col: 0 });
    }
    tokens.push(Token { kind: Kind::Eof, lit: "", line: line_num, col: 0 });
    tokens
}

/// The byte length of the match of `rule` at the start of `s`, if any.
fn match_rule(rule: Rule, s: &str) -> Option<usize> {
    match rule {
        Rule::Words(words) => words.iter().find(|w| s.starts_with(**w)).map(|w| w.len()),
        Rule::Str { open, close } => {
            let body = s.strip_prefix(open)?;
            quoted(body, close, false).map(|n| open.len() + n)
        }
        Rule::Var { open, close, first, rest } => {
            let body = s.strip_prefix(open)?;
            let n = word(body, first, rest)?;
            body[n..].starts_with(close).then(|| open.len() + n + close.len_utf8())
        }
        Rule::Function { open, close } => {
            let body = s.strip_prefix(open)?;
            let n = body.find(close)?;
            (n > 0).then(|| open.len() + n + close.len_utf8())
        }
        Rule::Type { open, close, first, rest } => {
            let body = s.strip_prefix(open)?;
            let mut n = 0;
            if let Some(args) = body.strip_prefix('(') {
                let end = args.find(')')?;
                if end == 0 {
                    return None;
                }
                n = end + 2;
            }
            let name = &body[n..];
            let m = word(name, first, rest)
                .filter(|&m| name[m..].starts_with(close))
                .or_else(|| package_path(name).filter(|&m| name[m..].starts_with(close)))?;
            Some(open.len() + n + m + close.len_utf8())
        }
        Rule::Template { prefix, close } => {
            let body = s.strip_prefix(prefix)?;
            quoted(body, close, true).map(|n| prefix.len() + n)
        }
        Rule::Compare(alts) => alts.iter().find_map(|alt| {
            let after = s.strip_prefix(alt.0)?;
            match alt.1 {
                None => Some(alt.0.len()),
                Some(b) => {
                    // Go's `\s`: [\t\n\f\r ]
                    let gap = after.bytes().take_while(|c| matches!(c, b'\t' | b'\n' | 0x0c | b'\r' | b' ')).count();
                    after[gap..].starts_with(b).then(|| alt.0.len() + gap + b.len())
                }
            }
        }),
        Rule::Chars(set) => s.chars().next().filter(|c| set.contains(*c)).map(|c| c.len_utf8()),
        Rule::Int => {
            let b = s.as_bytes();
            let mut n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            if n == 0 {
                return None;
            }
            if b.get(n) == Some(&b'.') {
                let frac = b[n + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                if frac > 0 {
                    n += 1 + frac;
                }
            }
            Some(n)
        }
        Rule::Ident { first, rest } => word(s, first, rest),
        Rule::Space(set) => {
            let n: usize = s.chars().take_while(|c| set.contains(*c)).map(char::len_utf8).sum();
            (n > 0).then_some(n)
        }
    }
}

/// `first rest*`
fn word(s: &str, first: Class, rest: Class) -> Option<usize> {
    let mut chars = s.char_indices();
    let (_, c) = chars.next()?;
    if !first.has(c) {
        return None;
    }
    Some(chars.find(|&(_, c)| !rest.has(c)).map_or(s.len(), |(i, _)| i))
}

/// The body of a quoted literal up to and including `close`: `\` escapes any
/// character; in a template a `{...}` group (no nested braces) may hold `close`.
fn quoted(body: &str, close: char, template: bool) -> Option<usize> {
    let mut it = body.char_indices();
    while let Some((i, c)) = it.next() {
        if c == close {
            return Some(i + c.len_utf8());
        }
        if c == '\\' {
            it.next()?;
        } else if template && c == '{' {
            loop {
                let (_, d) = it.next()?;
                if d == '}' {
                    break;
                }
                if d == '{' {
                    return None;
                }
            }
        }
    }
    None
}

/// `[a-z0-9-]+(\.[a-z0-9-]+)+(/[A-Za-z0-9][A-Za-z0-9._-]*){2,}` — a package path.
fn package_path(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let host = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-';
    let mut n = b.iter().take_while(|c| host(c)).count();
    if n == 0 {
        return None;
    }
    let mut dots = 0;
    while b.get(n) == Some(&b'.') {
        let m = b[n + 1..].iter().take_while(|c| host(c)).count();
        if m == 0 {
            break;
        }
        n += 1 + m;
        dots += 1;
    }
    if dots == 0 {
        return None;
    }
    let mut segments = 0;
    while b.get(n) == Some(&b'/') && b.get(n + 1).is_some_and(|c| c.is_ascii_alphanumeric()) {
        n += 2;
        n += b[n..].iter().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')).count();
        segments += 1;
    }
    (segments >= 2).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{HARI, KANADE};

    fn kinds(src: &str, p: &Profile) -> Vec<(Kind, String)> {
        tokenize(src, p).into_iter().map(|t| (t.kind, t.lit.to_string())).collect()
    }

    #[test]
    fn rule_order_decides() {
        use Kind::*;
        let t = kinds("'이름'을 \"하리\"로 정하자", &HARI);
        assert_eq!(
            t,
            vec![
                (Var, "'이름'".into()),
                (Particle, "을".into()),
                (Str, "\"하리\"".into()),
                (Particle, "로".into()),
                (KwMake, "정하자".into()),
                (Eof, "".into())
            ]
        );
        // 와 같다 is one comparison, 와 alone a particle.
        assert_eq!(kinds("'a'와 같다", &HARI)[1], (Compare, "와 같다".into()));
        assert_eq!(kinds("[github.com/a/b]", &HARI)[0], (Type, "[github.com/a/b]".into()));
        assert_eq!(kinds("[(숫자)목록]", &HARI)[0], (Type, "[(숫자)목록]".into()));
        assert_eq!(kinds("틀\"{'a'}\" ", &HARI)[0], (TemplateString, "틀\"{'a'}\"".into()));
        assert_eq!(kinds("もしくは", &KANADE)[0], (KwElif, "もしくは".into()));
    }

    #[test]
    fn indentation() {
        use Kind::*;
        let t: Vec<Kind> = tokenize("a:\n    b\n\n        c\nd", &HARI).into_iter().map(|t| t.kind).collect();
        assert_eq!(t, vec![Ident, Colon, Indent, Ident, Indent, Ident, Dedent, Dedent, Ident, Eof]);
    }
}
