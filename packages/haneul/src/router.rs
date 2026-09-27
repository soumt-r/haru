//! Route rules: `/사용자/<id>`, `/글/<int:번호>`, `/파일/<path:경로>`.
//!
//! A rule is split at `/` into fixed parts and variables. A variable matches
//! one part (`string`, the default), a whole number (`int`), a number
//! (`float`), or the rest of the path, slashes and all (`path`). A rule that
//! ends with `/` also answers the path without it, with a redirect to it.

use crate::data::V;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conv {
    Str,
    Int,
    Float,
    Path,
}

#[derive(Debug)]
enum Seg {
    Lit(String),
    Var(String, Conv),
}

#[derive(Debug)]
pub struct Rule {
    segs: Vec<Seg>,
    /// The rule ends with `/`.
    slash: bool,
}

pub enum Match {
    /// The variables' names and values.
    Yes(Vec<(String, V)>),
    /// The rule wants the path with a `/` at the end.
    NeedsSlash,
    No,
}

/// Why a rule cannot be read (`Err` of [`Rule::parse`]).
pub enum RuleError {
    NoLeadingSlash,
    BadVariable(String),
    UnknownConverter(String),
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule, RuleError> {
        let body = text.strip_prefix('/').ok_or(RuleError::NoLeadingSlash)?;
        let slash = body.ends_with('/');
        let body = body.strip_suffix('/').unwrap_or(body);
        let mut segs = Vec::new();
        if !body.is_empty() {
            for part in body.split('/') {
                segs.push(match part.strip_prefix('<').and_then(|p| p.strip_suffix('>')) {
                    Some(inner) => {
                        let (conv, name) = match inner.split_once(':') {
                            Some((c, n)) => (c.trim(), n.trim()),
                            None => ("string", inner.trim()),
                        };
                        let conv = match conv {
                            "string" | "str" | "글" | "文字列" => Conv::Str,
                            "int" | "정수" | "整数" => Conv::Int,
                            "float" | "number" | "숫자" | "数" => Conv::Float,
                            "path" | "경로" | "パス" => Conv::Path,
                            other => return Err(RuleError::UnknownConverter(other.to_string())),
                        };
                        if name.is_empty() || name.contains(['<', '>']) {
                            return Err(RuleError::BadVariable(part.to_string()));
                        }
                        Seg::Var(name.to_string(), conv)
                    }
                    None if part.contains(['<', '>']) => return Err(RuleError::BadVariable(part.to_string())),
                    None => Seg::Lit(part.to_string()),
                });
            }
        }
        if let Some(i) = segs.iter().position(|s| matches!(s, Seg::Var(_, Conv::Path))) {
            if i + 1 != segs.len() {
                return Err(RuleError::BadVariable(text.to_string()));
            }
        }
        Ok(Rule { segs, slash })
    }

    /// How general the rule is, to pick the most specific of several that
    /// match: fixed parts before variables, variables before `path`.
    pub fn weight(&self) -> Vec<u8> {
        self.segs
            .iter()
            .map(|s| match s {
                Seg::Lit(_) => 0,
                Seg::Var(_, Conv::Path) => 2,
                Seg::Var(..) => 1,
            })
            .collect()
    }

    /// Matches a decoded path (it starts with `/`).
    pub fn matches(&self, path: &str) -> Match {
        let body = path.strip_prefix('/').unwrap_or(path);
        let has_slash = body.ends_with('/');
        let trimmed = body.strip_suffix('/').unwrap_or(body);
        let parts: Vec<&str> = if trimmed.is_empty() { Vec::new() } else { trimmed.split('/').collect() };
        let mut vars = Vec::new();
        let mut i = 0;
        for seg in &self.segs {
            match seg {
                Seg::Var(name, Conv::Path) => {
                    if i >= parts.len() {
                        return Match::No;
                    }
                    let mut rest = parts[i..].join("/");
                    // `path` takes a slash at the end too.
                    if has_slash {
                        rest.push('/');
                    }
                    vars.push((name.clone(), V::str(&rest)));
                    return Match::Yes(vars);
                }
                _ if i >= parts.len() => return Match::No,
                Seg::Lit(text) => {
                    if parts[i] != text {
                        return Match::No;
                    }
                }
                Seg::Var(name, conv) => {
                    let p = parts[i];
                    let v = match conv {
                        Conv::Str if !p.is_empty() => V::str(p),
                        Conv::Int if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) => match p.parse::<f64>() {
                            Ok(n) => V::Num(n),
                            Err(_) => return Match::No,
                        },
                        Conv::Float if is_decimal(p) => match p.parse::<f64>() {
                            Ok(n) => V::Num(n),
                            Err(_) => return Match::No,
                        },
                        _ => return Match::No,
                    };
                    vars.push((name.clone(), v));
                }
            }
            i += 1;
        }
        if i != parts.len() {
            return Match::No;
        }
        match (self.slash, has_slash) {
            (a, b) if a == b => Match::Yes(vars),
            // The root has no slash to add; `/x/` asked for `/x` is not it.
            (true, false) => Match::NeedsSlash,
            _ => Match::No,
        }
    }
}

fn is_decimal(p: &str) -> bool {
    let mut dot = false;
    let mut digit = false;
    for c in p.bytes() {
        match c {
            b'0'..=b'9' => digit = true,
            b'.' if !dot => dot = true,
            _ => return false,
        }
    }
    digit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(m: Match) -> Option<Vec<String>> {
        match m {
            Match::Yes(v) => Some(v.into_iter().map(|(k, v)| format!("{k}={}", v.text(crate::data::Lang::Hari))).collect()),
            _ => None,
        }
    }

    fn rule(s: &str) -> Rule {
        Rule::parse(s).ok().unwrap()
    }

    #[test]
    fn rules_match_paths() {
        assert_eq!(vars(rule("/").matches("/")), Some(vec![]));
        assert_eq!(vars(rule("/사용자/<id>").matches("/사용자/하루")), Some(vec!["id=하루".into()]));
        assert_eq!(vars(rule("/글/<int:n>").matches("/글/42")), Some(vec!["n=42".into()]));
        assert!(vars(rule("/글/<int:n>").matches("/글/4a")).is_none());
        assert!(vars(rule("/글/<int:n>").matches("/글/-4")).is_none());
        assert_eq!(vars(rule("/값/<float:x>").matches("/값/1.5")), Some(vec!["x=1.5".into()]));
        assert_eq!(vars(rule("/파일/<path:p>").matches("/파일/a/b/c.txt")), Some(vec!["p=a/b/c.txt".into()]));
        assert!(vars(rule("/파일/<path:p>").matches("/파일")).is_none());
        assert!(vars(rule("/a").matches("/a/b")).is_none());
        assert!(vars(rule("/a/b").matches("/a")).is_none());
    }

    #[test]
    fn trailing_slashes() {
        assert!(matches!(rule("/글/").matches("/글"), Match::NeedsSlash));
        assert!(matches!(rule("/글/").matches("/글/"), Match::Yes(_)));
        assert!(matches!(rule("/글").matches("/글/"), Match::No));
    }

    #[test]
    fn bad_rules() {
        assert!(matches!(Rule::parse("글"), Err(RuleError::NoLeadingSlash)));
        assert!(matches!(Rule::parse("/<list:x>"), Err(RuleError::UnknownConverter(_))));
        assert!(matches!(Rule::parse("/<>"), Err(RuleError::BadVariable(_))));
        assert!(matches!(Rule::parse("/<path:p>/x"), Err(RuleError::BadVariable(_))));
    }
}
