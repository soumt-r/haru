//! What differs between Hari and Kanade: the lexer's rules (tried in order,
//! the first that matches wins — the order is part of the grammar) and the
//! handful of words the parser looks at (Hana's `LangProfile`).

use crate::token::Kind;

/// Character classes used by the rules.
#[derive(Clone, Copy, Debug)]
pub enum Class {
    /// `[가-힣a-zA-Z_]`
    HariStart,
    /// `[가-힣a-zA-Z0-9_]`
    HariRest,
    /// `[가-힣a-zA-Zぁ-んァ-ヶ一-龯ー_]`
    JaStart,
    /// `[가-힣a-zA-Zぁ-んァ-ヶ一-龯ー0-9_]`
    JaRest,
}

impl Class {
    #[inline]
    pub fn has(self, c: char) -> bool {
        let hangul = ('가'..='힣').contains(&c);
        let latin = c.is_ascii_alphabetic() || c == '_';
        let ja = ('ぁ'..='ん').contains(&c) || ('ァ'..='ヶ').contains(&c) || ('一'..='龯').contains(&c) || c == 'ー';
        match self {
            Class::HariStart => hangul || latin,
            Class::HariRest => hangul || latin || c.is_ascii_digit(),
            Class::JaStart => hangul || latin || ja,
            Class::JaRest => hangul || latin || ja || c.is_ascii_digit(),
        }
    }
}

/// One alternative of the comparison rule: `a`, or `a\s*b` when `b` is set.
#[derive(Clone, Copy, Debug)]
pub struct Cmp(pub &'static str, pub Option<&'static str>);

#[derive(Clone, Copy, Debug)]
pub enum Rule {
    /// Literal alternatives, tried in order (`^A|^B`).
    Words(&'static [&'static str]),
    /// `open (\\. | [^close\\])* close`
    Str { open: &'static str, close: char },
    /// `open first rest* close` (Hari allows a digit first: `'[...]+'`)
    Var { open: &'static str, close: char, first: Class, rest: Class },
    /// `open [^close]+ close`
    Function { open: &'static str, close: char },
    /// `open (\([^)]+\))? (ident | host.tld/owner/repo) close`
    Type { open: &'static str, close: char, first: Class, rest: Class },
    /// `prefix ( \{[^{}]*\} | \\. | [^close\\{] )* close`
    Template { prefix: &'static str, close: char },
    Compare(&'static [Cmp]),
    /// One of the characters.
    Chars(&'static str),
    /// `\d+(\.\d+)?`
    Int,
    Ident { first: Class, rest: Class },
    /// Skipped: `[chars]+`
    Space(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopKind {
    ForEach,
    While,
    Range,
}

pub struct Profile {
    pub name: &'static str,
    pub rules: &'static [(Kind, Rule)],
    /// A line is cut at the first of these (`(참고)`, `(참고:`).
    pub comment_markers: [&'static str; 2],

    pub error_literals: &'static [&'static str],
    pub plural_self_words: &'static [&'static str],
    pub condition_then_words: &'static [&'static str],
    pub popped_value_word: &'static str,
    pub member_particle: &'static str,
    pub type_in_word: &'static str,
    pub template_prefix: &'static str,
    pub template_suffix: &'static str,
    pub constructor_function_name: &'static str,
    pub front_marker: &'static str,
    /// Byte length of one delimiter of STRING/VAR/TYPE/FUNCTION.
    pub delim_len: usize,
    pub type_open: &'static str,
    pub type_close: &'static str,
    pub import_as_particles: &'static [&'static str],
    pub import_all_word: &'static str,

    pub private_suffix: &'static str,
    pub protected_suffix: &'static str,
    pub const_suffix: &'static str,
    pub abstract_prefix: &'static str,
    pub print_inline_suffix: &'static str,
    pub classify_loop: fn(verb: &str, components: &[crate::parser::Component]) -> LoopKind,
    pub compare_sov: fn(&str) -> String,
    pub compare_svo: fn(&str) -> String,
}

impl Profile {
    pub fn access_modifier(&self, verb: &str) -> &'static str {
        if verb.ends_with(self.private_suffix) {
            "private"
        } else if verb.ends_with(self.protected_suffix) {
            "protected"
        } else {
            "public"
        }
    }
}

use Kind::*;

static HARI_RULES: &[(Kind, Rule)] = &[
    (Str, Rule::Str { open: "\"", close: '"' }),
    (Var, Rule::Var { open: "'", close: '\'', first: Class::HariRest, rest: Class::HariRest }),
    (Function, Rule::Function { open: "<", close: '>' }),
    (Type, Rule::Type { open: "[", close: ']', first: Class::HariStart, rest: Class::HariRest }),
    (LBracket, Rule::Words(&["["])),
    (RBracket, Rule::Words(&["]"])),
    (LParen, Rule::Words(&["("])),
    (RParen, Rule::Words(&[")"])),
    (LBrace, Rule::Words(&["{"])),
    (RBrace, Rule::Words(&["}"])),
    (Comma, Rule::Words(&[","])),
    (Colon, Rule::Words(&[":"])),
    (KwReturn, Rule::Words(&["돌려주자"])),
    (KwBreak, Rule::Words(&["반복을 끝내자"])),
    (KwLoop, Rule::Words(&["반복하자"])),
    (KwPush, Rule::Words(&["추가하자"])),
    (KwPop, Rule::Words(&["빼내자", "꺼내자"])),
    (KwPopped, Rule::Words(&["꺼낸"])),
    (KwTry, Rule::Words(&["일단 해보자"])),
    (KwCatch, Rule::Words(&["발생했다면"])),
    (KwFinally, Rule::Words(&["마무리는 항상"])),
    (KwThrow, Rule::Words(&["던지자", "발생시키자"])),
    (KwClass, Rule::Words(&["설계하자", "밑설계하자"])),
    (KwInterface, Rule::Words(&["규정하자"])),
    (KwMustHave, Rule::Words(&["있어야 한다"])),
    (KwImport, Rule::Words(&["가져오자"])),
    (KwFrom, Rule::Words(&["에서"])),
    (KwImplements, Rule::Words(&["따르는"])),
    (KwIf, Rule::Words(&["만약"])),
    (KwElse, Rule::Words(&["그렇지 않다면", "그렇지 않고"])),
    (Ident, Rule::Words(&["라면"])),
    (
        KwMake,
        Rule::Words(&[
            "만들어 숨기자",
            "만들어 물려주자",
            "만들자",
            "정하여 숨기자",
            "정하여 물려주자",
            "정하자",
            "숨기자",
            "고정하자",
            "준비하자",
        ]),
    ),
    (KwPrint, Rule::Words(&["출력하자", "이어출력하자"])),
    (KwInput, Rule::Words(&["입력받자"])),
    (KwExecute, Rule::Words(&["실행하자"])),
    (KwNull, Rule::Words(&["비어있음"])),
    (KwTrue, Rule::Words(&["참"])),
    (KwFalse, Rule::Words(&["거짓"])),
    (KwConstruct, Rule::Words(&["처음 만들어질 때"])),
    (KwDoAs, Rule::Words(&["다음과 같이 하자"])),
    (KwBase, Rule::Words(&["바탕으로 하고", "바탕으로"])),
    (KwGetter, Rule::Words(&["가져올 때"])),
    (KwSetter, Rule::Words(&["정할 때"])),
    (KwParent, Rule::Words(&["부모"])),
    (KwOuter, Rule::Words(&["바깥"])),
    (KwNew, Rule::Words(&["새로운"])),
    (KwSwitch, Rule::Words(&["따라 나누자"])),
    (KwCase, Rule::Words(&["경우"])),
    (KwDefault, Rule::Words(&["나머지는"])),
    (KwFallthrough, Rule::Words(&["다음으로 이어가자"])),
    (KwAnd, Rule::Words(&["그리고"])),
    (KwOr, Rule::Words(&["또는"])),
    (KwSelf, Rule::Words(&["나"])),
    (KwFront, Rule::Words(&["앞에서", "앞에"])),
    (KwBack, Rule::Words(&["뒤에서", "뒤에"])),
    (KwAdd, Rule::Words(&["더하자"])),
    (KwSub, Rule::Words(&["빼자"])),
    (TemplateString, Rule::Template { prefix: "틀\"", close: '"' }),
    (
        Compare,
        Rule::Compare(&[
            Cmp("==", None),
            Cmp("!=", None),
            Cmp("<=", None),
            Cmp(">=", None),
            Cmp("<", None),
            Cmp(">", None),
            Cmp("와", Some("같다")),
            Cmp("과", Some("같다")),
            Cmp("보다", Some("크다")),
            Cmp("보다", Some("작다")),
            Cmp("이상이다", None),
            Cmp("이하이다", None),
            Cmp("이하다", None),
            Cmp("같지 않다", None),
            Cmp("같다", None),
            Cmp("다르다", None),
            Cmp("크다", None),
            Cmp("작다", None),
            Cmp("의", Some("일종이다")),
            Cmp("이다", None),
        ]),
    ),
    (Assign, Rule::Words(&["="])),
    (Op, Rule::Chars("+-*/%")),
    (TypeIn, Rule::Words(&["인"])),
    (
        Particle,
        Rule::Words(&[
            "를", "을", "가", "이", "는", "은", "의", "와", "과", "로", "으로", "에", "에서", "보다", "만큼", "도", "번째",
            "부터", "까지", "마다", "앞에서", "뒤에서", "앞에", "뒤에",
        ]),
    ),
    (Int, Rule::Int),
    (Ident, Rule::Ident { first: Class::HariStart, rest: Class::HariRest }),
    (Eof, Rule::Space(" \t")),
];

static KANADE_RULES: &[(Kind, Rule)] = &[
    (TemplateString, Rule::Template { prefix: "枠「", close: '」' }),
    (Str, Rule::Str { open: "「", close: '」' }),
    (Var, Rule::Var { open: "『", close: '』', first: Class::JaStart, rest: Class::JaRest }),
    (Function, Rule::Function { open: "〈", close: '〉' }),
    (Type, Rule::Type { open: "【", close: '】', first: Class::JaStart, rest: Class::JaRest }),
    (LBracket, Rule::Words(&["【"])),
    (RBracket, Rule::Words(&["】"])),
    (LParen, Rule::Words(&["("])),
    (RParen, Rule::Words(&[")"])),
    (LBrace, Rule::Words(&["{"])),
    (RBrace, Rule::Words(&["}"])),
    (Comma, Rule::Words(&[",", "、"])),
    (Colon, Rule::Words(&[":"])),
    (KwReturn, Rule::Words(&["返そう"])),
    (KwBreak, Rule::Words(&["繰り返しを終わろう"])),
    (KwLoop, Rule::Words(&["ごとに繰り返そう", "間繰り返そう", "まで繰り返そう"])),
    (KwPush, Rule::Words(&["追加しよう"])),
    (KwPop, Rule::Words(&["取り出そう"])),
    (KwPopped, Rule::Words(&["取り出した"])),
    (KwTry, Rule::Words(&["とりあえずやってみよう"])),
    (KwCatch, Rule::Words(&["発生したら", "発生したなら"])),
    (KwFinally, Rule::Words(&["最後はいつも", "締めくくりはいつも"])),
    (KwThrow, Rule::Words(&["発生させよう"])),
    (KwClass, Rule::Words(&["設計しよう", "下設計しよう"])),
    (KwInterface, Rule::Words(&["規定しよう"])),
    (KwMustHave, Rule::Words(&["なければならない"])),
    (KwImport, Rule::Words(&["持ってこよう"])),
    (KwFrom, Rule::Words(&["から"])),
    (KwImplements, Rule::Words(&["従う"])),
    (KwElif, Rule::Words(&["もしくは"])),
    (KwIf, Rule::Words(&["もし"])),
    (KwElse, Rule::Words(&["それ以外ならば", "それ以外なら", "それ以外で"])),
    (Ident, Rule::Words(&["ならば", "なら"])),
    (Ident, Rule::Words(&["値"])),
    (Ident, Rule::Words(&["全部"])),
    (
        KwMake,
        Rule::Words(&["作って隠そう", "作って譲ろう", "作ろう", "にしよう", "固定しよう", "隠そう", "譲ろう", "準備しよう"]),
    ),
    (KwPrint, Rule::Words(&["続けて出力しよう", "出力しよう"])),
    (KwInput, Rule::Words(&["入力させよう", "入力してもらおう"])),
    (KwExecute, Rule::Words(&["実行しよう"])),
    (KwNull, Rule::Words(&["空っぽ"])),
    (KwTrue, Rule::Words(&["真"])),
    (KwFalse, Rule::Words(&["偽"])),
    (KwConstruct, Rule::Words(&["最初に作られる時"])),
    (KwDoAs, Rule::Words(&["次のようにしよう"])),
    (KwBase, Rule::Words(&["基づいて", "もとにして"])),
    (KwGetter, Rule::Words(&["取得する時"])),
    (KwSetter, Rule::Words(&["決める時"])),
    (KwParent, Rule::Words(&["親"])),
    (KwOuter, Rule::Words(&["外"])),
    (KwNew, Rule::Words(&["新しい"])),
    (KwSwitch, Rule::Words(&["によって分けよう"])),
    (KwCase, Rule::Words(&["の場合"])),
    (KwDefault, Rule::Words(&["残りは"])),
    (KwFallthrough, Rule::Words(&["次に続けよう"])),
    (KwAnd, Rule::Words(&["かつ", "そして"])),
    (KwOr, Rule::Words(&["または"])),
    (KwSelf, Rule::Words(&["私"])),
    (KwFront, Rule::Words(&["前から", "前で", "前に", "前"])),
    (KwBack, Rule::Words(&["後ろから", "後ろで", "後ろに", "後から", "後で", "後に", "後"])),
    (KwAdd, Rule::Words(&["足そう"])),
    (KwSub, Rule::Words(&["引こう"])),
    (
        Compare,
        Rule::Compare(&[
            Cmp("==", None),
            Cmp("!=", None),
            Cmp("<=", None),
            Cmp(">=", None),
            Cmp("<", None),
            Cmp(">", None),
            Cmp("と同じだ", None),
            Cmp("と同じ", None),
            Cmp("と等しい", None),
            Cmp("より大きい", None),
            Cmp("より小さい", None),
            Cmp("以上だ", None),
            Cmp("以下だ", None),
            Cmp("以上", None),
            Cmp("以下", None),
            Cmp("の一種だ", None),
            Cmp("の一種", None),
            Cmp("一種だ", None),
            Cmp("一種", None),
            Cmp("同じだ", None),
            Cmp("同じ", None),
            Cmp("等しい", None),
            Cmp("異なる", None),
            Cmp("違う", None),
            Cmp("小さい", None),
            Cmp("大きい", None),
        ]),
    ),
    (Assign, Rule::Words(&["="])),
    (Op, Rule::Chars("+-*/%")),
    (
        Particle,
        Rule::Words(&[
            "を", "に", "で", "は", "が", "の", "と", "も", "から", "へ", "より", "くらい", "まで", "ずつ", "など", "番目",
        ]),
    ),
    (Int, Rule::Int),
    (Ident, Rule::Ident { first: Class::JaStart, rest: Class::JaRest }),
    (Eof, Rule::Space(" \t\u{3000}")),
];

fn hari_loop(_verb: &str, components: &[crate::parser::Component]) -> LoopKind {
    if components.len() == 1 {
        return LoopKind::ForEach;
    }
    if components.len() >= 2 {
        if let crate::ast::Expr::Identifier(id) = &components[components.len() - 1].expr {
            if id == "동안" || id == "동안은" {
                return LoopKind::While;
            }
        }
    }
    LoopKind::Range
}

fn hari_sov(op: &str) -> String {
    let has = |s| op.contains(s);
    let r = if has("일종이다") {
        "instanceof"
    } else if has("같다") && has("!") {
        "!="
    } else if has("같다") {
        "=="
    } else if has("크다") {
        ">"
    } else if has("작다") {
        "<"
    } else if has("이상이다") {
        ">="
    } else if has("이하이다") {
        "<="
    } else if has("다르다") || has("않다") {
        "!="
    } else {
        op
    };
    r.to_string()
}

fn hari_svo(op: &str) -> String {
    match op {
        "같다" => "==",
        "다르다" | "같지 않다" => "!=",
        _ => op,
    }
    .to_string()
}

fn kanade_loop(verb: &str, _components: &[crate::parser::Component]) -> LoopKind {
    if verb.starts_with("ごとに") {
        LoopKind::ForEach
    } else if verb.starts_with('間') {
        LoopKind::While
    } else {
        LoopKind::Range
    }
}

fn kanade_sov(op: &str) -> String {
    let has = |s| op.contains(s);
    let r = if has("一種") {
        "instanceof"
    } else if has("同じ") || has("等しい") {
        "=="
    } else if has("大きい") {
        ">"
    } else if has("小さい") {
        "<"
    } else if has("以上") {
        ">="
    } else if has("以下") {
        "<="
    } else if has("異なる") || has("違う") {
        "!="
    } else {
        op
    };
    r.to_string()
}

fn kanade_svo(op: &str) -> String {
    match op {
        "同じだ" | "同じ" | "等しい" => "==",
        "異なる" | "違う" => "!=",
        _ => op,
    }
    .to_string()
}

pub static HARI: Profile = Profile {
    name: "hari",
    rules: HARI_RULES,
    comment_markers: ["(참고)", "(참고:"],
    error_literals: &["오류", "오류가"],
    plural_self_words: &["우리", "'우리'"],
    condition_then_words: &["라면"],
    popped_value_word: "값",
    member_particle: "의",
    type_in_word: "인",
    template_prefix: "틀\"",
    template_suffix: "\"",
    constructor_function_name: "처음 만들어질 때",
    front_marker: "앞",
    delim_len: 1,
    type_open: "[",
    type_close: "]",
    import_as_particles: &["로", "으로"],
    import_all_word: "전부",
    private_suffix: "숨기자",
    protected_suffix: "물려주자",
    const_suffix: "고정하자",
    abstract_prefix: "밑",
    print_inline_suffix: "이어출력하자",
    classify_loop: hari_loop,
    compare_sov: hari_sov,
    compare_svo: hari_svo,
};

pub static KANADE: Profile = Profile {
    name: "kanade",
    rules: KANADE_RULES,
    comment_markers: ["(参考)", "(参考:"],
    error_literals: &["エラー", "エラーが"],
    plural_self_words: &["私たち", "『私たち』"],
    condition_then_words: &["なら", "ならば"],
    popped_value_word: "値",
    member_particle: "の",
    type_in_word: "の",
    template_prefix: "枠「",
    template_suffix: "」",
    constructor_function_name: "最初に作られる時",
    front_marker: "前",
    delim_len: 3,
    type_open: "【",
    type_close: "】",
    import_as_particles: &["に"],
    import_all_word: "全部",
    private_suffix: "隠そう",
    protected_suffix: "譲ろう",
    const_suffix: "固定しよう",
    abstract_prefix: "下",
    print_inline_suffix: "続けて出力しよう",
    classify_loop: kanade_loop,
    compare_sov: kanade_sov,
    compare_svo: kanade_svo,
};
