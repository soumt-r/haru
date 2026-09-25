//! Go's regular expressions (`regexp`, Perl flags) on Rust's `regex` crate.
//!
//! The two syntaxes look alike but differ: Go's \d \s \w \b are ASCII, `[` in
//! a class is a literal, `{,3}` is text, octal escapes and \Q...\E exist, and
//! each accepts things the other refuses. So a pattern is parsed here exactly
//! as Go's `regexp/syntax` parses it (accepting and refusing the same
//! patterns), and written out again in Rust's syntax with the same meaning:
//! every character as \x{..}, every class spelled out, every group explicit.
//! Matching is then Rust's, with the same leftmost-first rules as Go's.

use crate::regex_names::UNICODE_CLASSES;

/// The flags of Go's parser that change what a node means.
#[derive(Clone, Copy, Default)]
struct Flags {
    fold: bool,
    dot_nl: bool,
    multi_line: bool,
    non_greedy: bool,
}

enum Node {
    /// Rust syntax for one atom (a character, a class, an anchor).
    Atom(String),
    Group { capture: bool, alts: Vec<Vec<Node>> },
    Repeat { sub: Box<Node>, min: i32, max: i32, greedy: bool, counted: bool },
}

struct Frame {
    capture: bool,
    flags: Flags,
    alts: Vec<Vec<Node>>,
    cur: Vec<Node>,
}

/// Go's maximum tree height and repeat count.
const MAX_HEIGHT: usize = 1000;
const MAX_REPEAT: i32 = 1000;

/// The pattern in Rust's syntax, or `None` when Go refuses it.
pub fn translate(pattern: &str) -> Option<Translated> {
    let mut p = Parser { flags: Flags::default(), stack: Vec::new() };
    p.stack.push(Frame { capture: false, flags: p.flags, alts: Vec::new(), cur: Vec::new() });
    p.parse(pattern)?;
    if p.stack.len() != 1 {
        return None;
    }
    let mut top = p.stack.pop().unwrap();
    top.alts.push(top.cur);
    let root = Node::Group { capture: false, alts: top.alts };
    let height = height(&root);
    if height > MAX_HEIGHT {
        return None;
    }
    let mut out = String::new();
    emit(&root, &mut out);
    Some(Translated { pattern: out, height })
}

pub struct Translated {
    pub pattern: String,
    /// How deep the pattern nests (deep ones need a bigger stack to compile).
    pub height: usize,
}

struct Parser {
    flags: Flags,
    stack: Vec<Frame>,
}

/// Matches nothing (a surrogate, which Go takes but text never holds).
const NOTHING: &str = r"[^\x{0}-\x{10FFFF}]";

fn is_surrogate(c: u32) -> bool {
    (0xd800..0xe000).contains(&c)
}

/// A single character with the case flag.
fn literal(c: u32, fold: bool) -> Node {
    let atom = if is_surrogate(c) { NOTHING.to_string() } else { format!("\\x{{{c:X}}}") };
    Node::Atom(if fold { format!("(?i:{atom})") } else { atom })
}

/// `ranges` as the inside of a Rust class (surrogates left out).
fn ranges(r: &[(u32, u32)]) -> String {
    let mut out = String::new();
    for &(mut lo, mut hi) in r {
        if is_surrogate(lo) {
            lo = 0xe000;
        }
        if is_surrogate(hi) {
            hi = 0xd7ff;
        }
        if lo <= hi {
            out += &format!("\\x{{{lo:X}}}-\\x{{{hi:X}}}");
        }
    }
    out
}

const DIGIT: &[(u32, u32)] = &[(0x30, 0x39)];
const SPACE: &[(u32, u32)] = &[(0x9, 0xa), (0xc, 0xd), (0x20, 0x20)];
const WORD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5a), (0x5f, 0x5f), (0x61, 0x7a)];

/// Go's `\d \D \s \S \w \W`: (positive, ranges).
fn perl_group(c: u8) -> Option<(bool, &'static [(u32, u32)])> {
    Some(match c {
        b'd' => (true, DIGIT),
        b'D' => (false, DIGIT),
        b's' => (true, SPACE),
        b'S' => (false, SPACE),
        b'w' => (true, WORD),
        b'W' => (false, WORD),
        _ => return None,
    })
}

fn posix_group(name: &str) -> Option<(bool, &'static [(u32, u32)])> {
    let (positive, name) = match name.strip_prefix('^') {
        Some(n) => (false, n),
        None => (true, name),
    };
    let r: &'static [(u32, u32)] = match name {
        "alnum" => &[(0x30, 0x39), (0x41, 0x5a), (0x61, 0x7a)],
        "alpha" => &[(0x41, 0x5a), (0x61, 0x7a)],
        "ascii" => &[(0x0, 0x7f)],
        "blank" => &[(0x9, 0x9), (0x20, 0x20)],
        "cntrl" => &[(0x0, 0x1f), (0x7f, 0x7f)],
        "digit" => DIGIT,
        "graph" => &[(0x21, 0x7e)],
        "lower" => &[(0x61, 0x7a)],
        "print" => &[(0x20, 0x7e)],
        "punct" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)],
        "space" => &[(0x9, 0xd), (0x20, 0x20)],
        "upper" => &[(0x41, 0x5a)],
        "word" => WORD,
        "xdigit" => &[(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)],
        _ => return None,
    };
    Some((positive, r))
}

/// A group as a class item: nested and negated when needed.
fn group_item(positive: bool, r: &[(u32, u32)]) -> String {
    if positive {
        ranges(r)
    } else {
        format!("[^{}]", ranges(r))
    }
}

/// regexp/syntax's `canonicalName`.
fn canonical(name: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for c in name.bytes() {
        let c = match c {
            b'_' | b'-' | b' ' => continue,
            c if first => {
                first = false;
                c.to_ascii_uppercase()
            }
            c => c.to_ascii_lowercase(),
        };
        out.push(c as char);
    }
    out
}

fn next_char(s: &str) -> Option<(char, &str)> {
    let c = s.chars().next()?;
    Some((c, &s[c.len_utf8()..]))
}

impl Parser {
    fn frame(&mut self) -> &mut Frame {
        self.stack.last_mut().unwrap()
    }

    fn push(&mut self, n: Node) {
        self.frame().cur.push(n);
    }

    fn parse(&mut self, s: &str) -> Option<()> {
        let mut t = s;
        // The text of the repetition operator just read (Go refuses two in a row).
        let mut last_repeat = false;
        while !t.is_empty() {
            let mut repeat = false;
            let b = t.as_bytes();
            match b[0] {
                b'(' => {
                    if b.len() >= 2 && b[1] == b'?' {
                        t = self.perl_flags(t)?;
                    } else {
                        let flags = self.flags;
                        self.stack.push(Frame { capture: true, flags, alts: Vec::new(), cur: Vec::new() });
                        t = &t[1..];
                    }
                }
                b'|' => {
                    let f = self.frame();
                    let cur = std::mem::take(&mut f.cur);
                    f.alts.push(cur);
                    t = &t[1..];
                }
                b')' => {
                    if self.stack.len() < 2 {
                        return None;
                    }
                    let mut f = self.stack.pop().unwrap();
                    f.alts.push(f.cur);
                    self.flags = f.flags;
                    self.push(Node::Group { capture: f.capture, alts: f.alts });
                    t = &t[1..];
                }
                b'^' => {
                    self.push(Node::Atom(if self.flags.multi_line { "(?m:^)" } else { "\\A" }.into()));
                    t = &t[1..];
                }
                b'$' => {
                    self.push(Node::Atom(if self.flags.multi_line { "(?m:$)" } else { "\\z" }.into()));
                    t = &t[1..];
                }
                b'.' => {
                    self.push(Node::Atom(if self.flags.dot_nl { "(?s:.)" } else { "[^\\n]" }.into()));
                    t = &t[1..];
                }
                b'[' => t = self.class(t)?,
                b'*' | b'+' | b'?' => {
                    let (min, max) = match b[0] {
                        b'*' => (0, -1),
                        b'+' => (1, -1),
                        _ => (0, 1),
                    };
                    t = self.repeat(min, max, false, &t[1..], last_repeat)?;
                    repeat = true;
                }
                b'{' => match parse_repeat(t) {
                    None => {
                        // Not a repetition: the brace is itself.
                        self.push(literal('{' as u32, self.flags.fold));
                        t = &t[1..];
                    }
                    Some((min, max, after)) => {
                        if min < 0 || min > MAX_REPEAT || max > MAX_REPEAT || (max >= 0 && min > max) {
                            return None;
                        }
                        t = self.repeat(min, max, true, after, last_repeat)?;
                        repeat = true;
                    }
                },
                b'\\' => t = self.escape(t)?,
                _ => {
                    let (c, rest) = next_char(t)?;
                    self.push(literal(c as u32, self.flags.fold));
                    t = rest;
                }
            }
            last_repeat = repeat;
        }
        Some(())
    }

    fn repeat<'s>(&mut self, min: i32, max: i32, counted: bool, after: &'s str, last_repeat: bool) -> Option<&'s str> {
        let mut greedy = !self.flags.non_greedy;
        let mut after = after;
        if let Some(rest) = after.strip_prefix('?') {
            after = rest;
            greedy = !greedy;
        }
        if last_repeat {
            return None;
        }
        let sub = self.frame().cur.pop()?;
        let node = Node::Repeat { sub: Box::new(sub), min, max, greedy, counted };
        if counted && (min >= 2 || max >= 2) && !repeat_is_valid(&node, MAX_REPEAT) {
            return None;
        }
        self.push(node);
        Some(after)
    }

    /// `(?flags)`, `(?flags:`, `(?P<name>` and `(?<name>`.
    fn perl_flags<'s>(&mut self, s: &'s str) -> Option<&'s str> {
        let b = s.as_bytes();
        let starts_p = b.len() > 4 && b[2] == b'P' && b[3] == b'<';
        let starts_name = b.len() > 3 && b[2] == b'<';
        if starts_p || starts_name {
            let start = if starts_name { 3 } else { 4 };
            let end = s.find('>')?;
            let name = s.get(start..end)?;
            if name.is_empty() || !name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
                return None;
            }
            let flags = self.flags;
            self.stack.push(Frame { capture: true, flags, alts: Vec::new(), cur: Vec::new() });
            return Some(&s[end + 1..]);
        }
        let mut t = &s[2..];
        let mut flags = self.flags;
        let mut negated = false;
        let mut saw_flag = false;
        while let Some((c, rest)) = next_char(t) {
            t = rest;
            match c {
                'i' => flags.fold = !negated,
                'm' => flags.multi_line = !negated,
                's' => flags.dot_nl = !negated,
                'U' => flags.non_greedy = !negated,
                '-' => {
                    if negated {
                        return None;
                    }
                    negated = true;
                    saw_flag = false;
                    continue;
                }
                ':' | ')' => {
                    if negated && !saw_flag {
                        return None;
                    }
                    if c == ':' {
                        let old = self.flags;
                        self.stack.push(Frame { capture: false, flags: old, alts: Vec::new(), cur: Vec::new() });
                    }
                    self.flags = flags;
                    return Some(t);
                }
                _ => return None,
            }
            saw_flag = true;
        }
        None
    }

    /// Everything that starts with a backslash outside a class.
    fn escape<'s>(&mut self, t: &'s str) -> Option<&'s str> {
        let b = t.as_bytes();
        if b.len() >= 2 {
            let anchor = match b[1] {
                b'A' => Some("\\A"),
                b'b' => Some("(?-u:\\b)"),
                b'B' => Some("(?-u:\\B)"),
                b'z' => Some("\\z"),
                b'C' => return None,
                b'Q' => {
                    let body = &t[2..];
                    let (lit, rest) = match body.find("\\E") {
                        Some(i) => (&body[..i], &body[i + 2..]),
                        None => (body, ""),
                    };
                    for c in lit.chars() {
                        self.push(literal(c as u32, self.flags.fold));
                    }
                    return Some(rest);
                }
                _ => None,
            };
            if let Some(a) = anchor {
                self.push(Node::Atom(a.into()));
                return Some(&t[2..]);
            }
            if b[1] == b'p' || b[1] == b'P' {
                let (item, rest) = unicode_class(t)?;
                self.push(self.class_atom(&item, false));
                return Some(rest);
            }
            if let Some((positive, r)) = perl_group(b[1]) {
                let item = group_item(positive, r);
                self.push(self.class_atom(&item, false));
                return Some(&t[2..]);
            }
        }
        let (c, rest) = parse_escape(t)?;
        self.push(literal(c as u32, self.flags.fold));
        Some(rest)
    }

    fn class_atom(&self, items: &str, negated: bool) -> Node {
        let class = match (items.is_empty(), negated) {
            (true, false) => NOTHING.to_string(),
            (true, true) => r"[\x{0}-\x{10FFFF}]".to_string(),
            _ => format!("[{}{items}]", if negated { "^" } else { "" }),
        };
        Node::Atom(if self.flags.fold { format!("(?i:{class})") } else { class })
    }

    fn class<'s>(&mut self, s: &'s str) -> Option<&'s str> {
        let mut t = &s[1..];
        let negated = t.starts_with('^');
        if negated {
            t = &t[1..];
        }
        let mut items = String::new();
        let mut first = true;
        while t.is_empty() || !t.starts_with(']') || first {
            first = false;
            if t.starts_with("[:") && t.len() > 2 {
                if let Some(i) = t[2..].find(":]") {
                    let name = &t[2..2 + i];
                    let (positive, r) = posix_group(name)?;
                    items += &group_item(positive, r);
                    t = &t[2 + i + 2..];
                    continue;
                }
            }
            if t.starts_with("\\p") || t.starts_with("\\P") {
                let (item, rest) = unicode_class(t)?;
                items += &item;
                t = rest;
                continue;
            }
            if t.len() >= 2 && t.as_bytes()[0] == b'\\' {
                if let Some((positive, r)) = perl_group(t.as_bytes()[1]) {
                    items += &group_item(positive, r);
                    t = &t[2..];
                    continue;
                }
            }
            let (lo, rest) = class_char(t)?;
            t = rest;
            let mut hi = lo;
            if t.len() >= 2 && t.starts_with('-') && !t[1..].starts_with(']') {
                let (h, rest) = class_char(&t[1..])?;
                if h < lo {
                    return None;
                }
                hi = h;
                t = rest;
            }
            items += &ranges(&[(lo, hi)]);
        }
        let node = self.class_atom(&items, negated);
        self.push(node);
        Some(&t[1..])
    }
}

/// One character of a class: itself or an escape.
fn class_char(t: &str) -> Option<(u32, &str)> {
    if t.starts_with('\\') {
        return parse_escape(t);
    }
    next_char(t).map(|(c, rest)| (c as u32, rest))
}

/// `\p{Name}`, `\pL`, `\P...`, `\p{^Name}`: the class item for Rust.
fn unicode_class(s: &str) -> Option<(String, &str)> {
    let mut positive = s.as_bytes()[1] == b'p';
    let (c, t) = next_char(&s[2..])?;
    let (name, rest) = if c != '{' {
        (&s[2..2 + c.len_utf8()], t)
    } else {
        let end = s.find('}')?;
        (&s[3..end], &s[end + 1..])
    };
    let name = match name.strip_prefix('^') {
        Some(n) => {
            positive = !positive;
            n
        }
        None => name,
    };
    let canon = canonical(name);
    let (_, class) = UNICODE_CLASSES.iter().find(|(n, _)| *n == canon)?;
    Some((if positive { class.to_string() } else { format!("[^{class}]") }, rest))
}

/// Go's `parseEscape`: one escaped character.
fn parse_escape(s: &str) -> Option<(u32, &str)> {
    let (c, mut t) = next_char(&s[1..])?;
    match c {
        '1'..='7' if !t.as_bytes().first().is_some_and(|d| (b'0'..=b'7').contains(d)) => None,
        '0'..='7' => {
            let mut r = c as u32 - '0' as u32;
            for _ in 1..3 {
                match t.as_bytes().first() {
                    Some(d @ b'0'..=b'7') => {
                        r = r * 8 + (d - b'0') as u32;
                        t = &t[1..];
                    }
                    _ => break,
                }
            }
            Some((r, t))
        }
        'x' => {
            let (c, rest) = next_char(t)?;
            if c == '{' {
                let end = rest.find('}')?;
                let digits = &rest[..end];
                if digits.is_empty() || !digits.bytes().all(|d| d.is_ascii_hexdigit()) {
                    return None;
                }
                let mut r: u32 = 0;
                for d in digits.chars() {
                    r = r * 16 + d.to_digit(16).unwrap();
                    if r > 0x10ffff {
                        return None;
                    }
                }
                return Some((r, &rest[end + 1..]));
            }
            let x = c.to_digit(16)?;
            let (c2, rest) = next_char(rest)?;
            let y = c2.to_digit(16)?;
            Some((x * 16 + y, rest))
        }
        'a' => Some((7, t)),
        'f' => Some((0xc, t)),
        'n' => Some((0xa, t)),
        'r' => Some((0xd, t)),
        't' => Some((9, t)),
        'v' => Some((0xb, t)),
        c if c.is_ascii() && !c.is_ascii_alphanumeric() => Some((c as u32, t)),
        _ => None,
    }
}

/// `{n}`, `{n,}` or `{n,m}` and what follows; min is -1 for a number too
/// large (Go's `parseRepeat`).
fn parse_repeat(s: &str) -> Option<(i32, i32, &str)> {
    let s = s.strip_prefix('{')?;
    let (min, s) = parse_int(s)?;
    let (max, s) = if let Some(rest) = s.strip_prefix(',') {
        if rest.starts_with('}') {
            (-1, rest)
        } else {
            let (max, rest) = parse_int(rest)?;
            (max, rest)
        }
    } else {
        (min, s)
    };
    let rest = s.strip_prefix('}')?;
    let min = if max == -2 { -1 } else { min };
    Some((min, if max == -2 { 0 } else { max }, rest))
}

/// Digits without a leading zero; -2 when the number is too large.
fn parse_int(s: &str) -> Option<(i32, &str)> {
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_digit() || (b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit()) {
        return None;
    }
    let len = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut n: i64 = 0;
    for &d in &b[..len] {
        if n >= 100_000_000 {
            n = -2;
            break;
        }
        n = n * 10 + (d - b'0') as i64;
    }
    Some((n as i32, &s[len..]))
}

/// Go's `repeatIsValid`: counted repetitions nested inside each other may
/// not multiply past n.
fn repeat_is_valid(node: &Node, mut n: i32) -> bool {
    match node {
        Node::Repeat { sub, min, max, counted, .. } => {
            if *counted {
                let mut m = *max;
                if m == 0 {
                    return true;
                }
                if m < 0 {
                    m = *min;
                }
                if m > n {
                    return false;
                }
                if m > 0 {
                    n /= m;
                }
            }
            repeat_is_valid(sub, n)
        }
        Node::Group { alts, .. } => alts.iter().flatten().all(|x| repeat_is_valid(x, n)),
        Node::Atom(_) => true,
    }
}

/// The height of Go's tree for the same pattern (close enough: Go merges
/// some nodes this tree keeps apart).
fn height(node: &Node) -> usize {
    match node {
        Node::Atom(_) => 1,
        Node::Repeat { sub, .. } => 1 + height(sub),
        Node::Group { capture, alts } => {
            let concat = |items: &Vec<Node>| match items.len() {
                0 => 1,
                1 => height(&items[0]),
                _ => 1 + items.iter().map(height).max().unwrap(),
            };
            let body = match alts.len() {
                1 => concat(&alts[0]),
                _ => 1 + alts.iter().map(concat).max().unwrap_or(1),
            };
            body + *capture as usize
        }
    }
}

fn emit(node: &Node, out: &mut String) {
    match node {
        Node::Atom(a) => out.push_str(a),
        Node::Group { capture, alts } => {
            out.push_str(if *capture { "(" } else { "(?:" });
            for (i, alt) in alts.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                for n in alt {
                    emit(n, out);
                }
            }
            out.push(')');
        }
        // Rust drops the groups inside x{0}; a part that can never match
        // keeps them (unset), as in Go.
        Node::Repeat { sub, max: 0, .. } => {
            out.push_str("(?:");
            out.push_str(NOTHING);
            emit(sub, out);
            out.push_str(")?");
        }
        Node::Repeat { sub, min, max, greedy, .. } => {
            out.push_str("(?:");
            emit(sub, out);
            out.push(')');
            match (*min, *max) {
                (0, -1) => out.push('*'),
                (1, -1) => out.push('+'),
                (0, 1) => out.push('?'),
                (n, -1) => out.push_str(&format!("{{{n},}}")),
                (n, m) if n == m => out.push_str(&format!("{{{n}}}")),
                (n, m) => out.push_str(&format!("{{{n},{m}}}")),
            }
            if !greedy {
                out.push('?');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::translate;

    fn groups(p: &str) -> usize {
        regex::Regex::new(&translate(p).unwrap().pattern).unwrap().captures_len()
    }

    #[test]
    fn groups_inside_a_zero_repeat_still_count() {
        assert_eq!(groups("(a){0}b"), 2);
        assert_eq!(groups("(?:(a)|(b)){0}"), 3);
        assert_eq!(groups("((a){0}){0}"), 3);
    }

    #[test]
    fn go_refuses_what_go_refuses() {
        for p in [r"\1", "a**", "(?x)", r"\Z", "[z-a]", "x{1001}", "(a{2}){501}", r"\p{Anatolian_Hieroglyphs}", "(", ")", "*"] {
            assert!(translate(p).is_none(), "{p}");
        }
        for p in [r"\8" ] {
            assert!(translate(p).is_none(), "{p}");
        }
        for p in ["a{,3}", "[[a]", r"\Qa*", "(?i)", r"\p{greek}", r"\x{D800}", "a(?i)*", "(a{2}){500}", "x{1000}"] {
            assert!(translate(p).is_some(), "{p}");
        }
    }
}
