//! The words the runtime itself uses in each language (Hana's `LangConfig`):
//! how values print, type names, built-in function names, error language.

use haru_syntax::Profile;

pub struct Lang {
    pub name: &'static str,
    pub syntax: &'static Profile,
    /// Error catalog column: 1 Korean, 2 Japanese.
    pub locale: usize,

    pub null: &'static str,
    pub true_word: &'static str,
    pub false_word: &'static str,
    pub object_format: (&'static str, &'static str),

    pub type_number: &'static str,
    pub type_string: &'static str,
    pub type_boolean: &'static str,
    pub type_any: &'static str,
    pub type_list: &'static str,
    pub type_dict: &'static str,
    pub type_null: &'static str,

    /// `<문자로>`, `<숫자로>`, `<코드로>`, `<글자로>`: the order of [`crate::builtins::Builtin`].
    pub builtins: [&'static str; 4],
    pub default_item: &'static str,
    pub default_index: &'static str,
    pub self_words: &'static [&'static str],
    pub plural_self_words: &'static [&'static str],
    pub length_word: &'static str,
    pub list_clear: &'static str,
    pub string_slice: &'static str,
    pub string_replace: &'static str,
    pub string_split: &'static str,
    pub string_contains: &'static str,
    pub var_quote: (&'static str, &'static str),
}

pub static HARI: Lang = Lang {
    name: "hari",
    syntax: &haru_syntax::HARI,
    locale: 1,
    null: "비어있음",
    true_word: "참",
    false_word: "거짓",
    object_format: ("[", " 객체]"),
    type_number: "숫자",
    type_string: "문자열",
    type_boolean: "논리",
    type_any: "아무거나",
    type_list: "목록",
    type_dict: "사전",
    type_null: "비어있음",
    builtins: ["문자로", "숫자로", "코드로", "글자로"],
    default_item: "아이템",
    default_index: "인덱스",
    self_words: &["나"],
    plural_self_words: &["우리"],
    length_word: "길이",
    list_clear: "비우기",
    string_slice: "자르기",
    string_replace: "바꾸기",
    string_split: "분리하기",
    string_contains: "포함확인",
    var_quote: ("'", "'"),
};

pub static KANADE: Lang = Lang {
    name: "kanade",
    syntax: &haru_syntax::KANADE,
    locale: 2,
    null: "空っぽ",
    true_word: "真",
    false_word: "偽",
    object_format: ("[", " オブジェクト]"),
    type_number: "数字",
    type_string: "文字列",
    type_boolean: "論理",
    type_any: "何でも",
    type_list: "リスト",
    type_dict: "辞書",
    type_null: "空っぽ",
    builtins: ["文字列に", "数字に", "コードに", "文字に"],
    default_item: "アイテム",
    default_index: "インデックス",
    self_words: &["私"],
    plural_self_words: &["私たち"],
    length_word: "長さ",
    list_clear: "空にする",
    string_slice: "切り取り",
    string_replace: "入れ替え",
    string_split: "分割",
    string_contains: "含むか確認",
    var_quote: ("『", "』"),
};

/// The language of a source file, by extension.
pub fn for_path(path: &std::path::Path) -> &'static Lang {
    match path.extension().and_then(|e| e.to_str()) {
        Some("knd") => &KANADE,
        _ => &HARI,
    }
}
