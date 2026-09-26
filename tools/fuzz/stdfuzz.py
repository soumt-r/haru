"""Writes random Hari programs that call the standard modules, for
differential testing against Hana (tools/runcheck.sh).

    python tools/fuzz/stdfuzz.py <out dir> <count> [seed] [modules]

`modules` is a comma list of the modules below (default: all of them). Each
call runs inside 일단 해보자 so a failing call prints its message and the
program goes on; now and then one is left uncaught. Arguments are mostly of
the kind the function takes, sometimes of another kind, and now and then
there is one too few or too many. Callbacks cover the paths where a function
of the program runs inside a native one: errors, breaks, wrong answers,
wrong argument counts, nested standard calls.
"""
import os
import random
import sys

# The functions of each module: Hari name and the kinds of its arguments
# (s text, n number, i whole number, l list, L list of text, f function,
# a anything; a trailing '?' marks an optional argument).
MODULES = {
    "텍스트": [
        ("대문자", "s"), ("소문자", "s"), ("다듬기", "s"),
        ("왼쪽채우기", "sic"), ("오른쪽채우기", "sic"), ("반복", "si"),
        ("거꾸로", "s"), ("시작하는지", "ss"), ("끝나는지", "ss"),
        ("잇기", "Ls"), ("세기", "ss"), ("위치", "ss"),
    ],
    "목록": [
        ("정렬", "l"), ("뒤집기", "l"), ("중복제거", "l"), ("범위", "ii?i"),
        ("평탄화", "l"), ("조각내기", "li"), ("짝짓기", "ll"),
        ("변환하기", "lf"), ("걸러내기", "lf"), ("접기", "lfa"), ("찾기", "lf"),
        ("하나라도", "lf"), ("모두", "lf"), ("기준정렬", "lf"),
    ],
    "JSON": [("파싱", "j"), ("문자열화", "v?i")],
    "CSV": [("파싱", "C?d"), ("문자열화", "R?d")],
    "통계": [("합계", "N"), ("최솟값", "N"), ("최댓값", "N"), ("평균", "N"), ("중앙값", "N"), ("표준편차", "N")],
    "경로": [("합치기", "Ps?P?P?"), ("폴더이름", "P"), ("파일이름", "P"), ("확장자", "P"), ("확장자뺀이름", "P"),
             ("확장자바꾸기", "Ps"), ("정리하기", "P"), ("절대경로인지", "P"), ("부분나누기", "P")],
    "인코딩": [("베이스64인코딩", "s"), ("베이스64디코딩", "B"), ("주소인코딩", "s"), ("주소디코딩", "U")],
    "해시": [("SHA256", "s")],
    # Random results differ between runs: only calls whose output does not.
    "무작위": [("정수", "!"), ("실수", "!"), ("고르기", "!"), ("섞기", "!"), ("고유번호", "!")],
    "날짜": [("지금", "!"), ("서식", "DF"), ("읽기", "YF"), ("요일", "D"), ("기다리기", "!")],
    "timezone": [("시간대오프셋", "DZ"), ("시간대서식", "DFZ"), ("시간대읽기", "YFZ"), ("시간대요일", "DZ"),
                 ("네이티브_Offset", "aa?a"), ("네이티브_Format", "aa?a?a"), ("네이티브_Parse", "Y?F?Z?a"), ("네이티브_Weekday", "D?Z?a")],
    "정규식": [("검사", "TX"), ("찾기", "TX"), ("그룹", "TX"), ("모두찾기", "TX"), ("치환", "TXE"), ("분할", "TX")],
}

# Pieces of Go regular expressions (as Hari source: \" is a quote), valid
# and not.
RX_ATOMS = ["a", "b", "가", "k", "K", "s", "1", "_", " ", "-", ".", "^", "$", r"\d", r"\D", r"\w", r"\W", r"\s", r"\S",
            r"\b", r"\B", r"\.", r"\*", r"\x41", r"\x{AC00}", r"\x{D800}", r"\101", r"\0", r"\1", r"\8", r"\Q.*\E",
            r"\Qa", r"\pL", r"\p{Hangul}", r"\PL", r"\p{^Greek}", r"\p{greek}", r"\p{Anatolian_Hieroglyphs}", r"\pZ",
            r"\p{Letter}", r"\p{Lc}", r"\p{ASCII}", r"\p{Nope}", r"\A", r"\z", r"\Z", r"\C", r"\<", r"\_", r"\q",
            r"\a", r"\v", r"\f", "[abc]", "[^a-c]", "[a-]", "[]a]", "[[:alpha:]]", "[[:^digit:]]", r"[\d\s]", "[가-힣]",
            r"[a-\d]", "[z-a]", r"[\w-]", "[[:foo:]]", "[[a]", "[^]a]", "[", "[a", r"[\pL\p{Hangul}]", r"[^\x00-\x{10FFFF}]",
            "[&&a]", "[a~~b]", "[a--b]", r"[\b]", r"[\Q]", "{", "{,3}", "}", r"\p", r"\p{", r"\x{}", r"\x{110000}", r"\xZZ",
            r"\x4", "ſ", "K"]
RX_QUANT = ["*", "+", "?", "{2}", "{1,3}", "{2,}", "*?", "+?", "??", "{1,2}?", "**", "{1001}", "{0}", "{01}",
            "{2}{3}", "*+", "{3,1}", "{999999999}", "{1000}"]
REGEX_TEXTS = [r'"abc 123 가나다\nABC_def"', '"aaa"', '""', '"a1b2c3"', '"Hello World"', '"ſK k s S"',
               '"x{,3}y"', '"가나다라 마바사"', '"a.b*c"', r'"line1\nline2\n"', '"___"', '"αβγ ΑΒΓ"', '"  "',
               '"1234567890"', '"aAbBkKsS"', '"[]{}()"', r'"tab\there"', '"a-b_c d"', '"éé"']
REPLACEMENTS = ['"$1"', '"$$"', '"[$0]"', '"$2x"', '"$10"', '"$"', '"$a"', '"${1}"', '"<$0$1>"', '""', '"X"', '"$9$01"']

SECONDS = ["0", "1700000000", "1710054000", "1730613600", "1699999999.5", "(0 - 2208988800)", "비어있음", "(0 - 1)", "1700000000.9", "(0 - 86400.5)", "951782400", "8640000000000",
           "8640000000001", "(0 - 8640000000000)", "(0 - 62135596800)", "(0 - 62167219200)", "253402300799",
           "253402300800", "(1 / 0)", '"1"', "4102444800"]
LAYOUTS = ['"YYYY-MM-DD HH:mm:ss"', '"YYYY년 MM월 DD일"', '"HHmmss"', '"YYYYY"', '"MMM DDD"', '""', '"yyyy"',
           '"DD/MM/YYYY"', '"ss초"', '"YYYYMMDD"', '"HH:mm"', '"년YYYY"']
DATE_TEXTS = ['"2024-03-10 02:30:00"', '"2024-11-03 01:30:00"', '"1988-05-08 02:30:00"', '"2024-10-27 01:30:00"', '"2024-02-29 12:30:00"', '"2023-02-29 00:00:00"', '"2024년 03월 01일"', '"123456"', '"1970-01-01 00:00:00"',
              '"0000-01-01 00:00:00"', '"9999-12-31 23:59:59"', '"2024-13-01 00:00:00"', '"2024-00-10 00:00:00"',
              '"2024-01-01 24:00:00"', '"2024-1-1 0:0:0"', '"31/12/1999"', '"20240101"', '"12:60"', '""', '"23:59"',
              '"년2024"', '"２０２４"', '"2024-01-01 00:00:00 "', '"00초"', '"61초"']
ZONES = ['"Asia/Seoul"', '"UTC"', '"Z"', '"America/New_York"', '"Europe/London"', '"+09:00"', '"-0530"', '"+24:00"',
         '"+09:60"', '"asia/seoul"', '"Local"', '""', '"Nowhere/City"', '"Australia/Lord_Howe"', '"Asia/Kathmandu"',
         '"Etc/GMT+5"', '"US/Pacific"', '"America/Sao_Paulo"', '"../etc"', '"+0900"', '"+9:00"', '"한국"', "비어있음", "3"]

SPECIAL_CALLS = {
    "날짜": ["(<지금>() > 1700000000)", "<지금>(1)", "<기다리기>(0)", "<기다리기>((0 - 1))", "<기다리기>(3601)",
             '<기다리기>("1")', "<기다리기>(0.001)", "<기다리기>()"],
}

RANDOM_CALLS = ["<정수>(3, 3)", "<정수>(5, 1)", "<정수>(1.5, 2)", "<정수>(1)", "<고르기>([7])", "<고르기>([])",
                '<고르기>("a")', "<섞기>([4])", "<섞기>([])", "<섞기>(1)", "(<실수>() < 1)", "(<실수>() >= 0)",
                "<실수>(1)", "(<정수>(0, 1) < 2)", "<정수>((9007199254740992 * 2), 0)", "(<고유번호>() == <고유번호>())",
                "<고유번호>(1)", "<정수>((0 - 3), (0 - 3))", '<고르기>(["하나"])']
SPECIAL_CALLS["무작위"] = RANDOM_CALLS

# Imports under another name where two modules share a function name.
PREFIX = {"JSON": "J", "CSV": "C", "정규식": "R"}

TEXTS = ['""', '"abc"', '"가나다"', '"  공백 섞인  "', '"ß와 ǅ"', '"aXaXa"', '"a"', '"X"',
         '"Hello World"', r'"\t탭\n"','"ǰᾳİ"', '"123"', '"하리 하리"', '"*"', '"ab"']
NUMS = ["0", "1", "2", "3", "5", "(0 - 1)", "(0 - 3)", "1.5", "10", "0.5", "2000000",
        "(0 - 0)", "(1 / 3)", "7"]
LISTS = ["[3, 1, 2]", '["나", "가", "다"]', "[1, \"가\"]", "[]", "[[1, 2], 3, [4]]",
         "[1, 1, 2, 2, 3]", '["a", "b", "a"]', "[참, 거짓, 참, 비어있음, 비어있음]", "[2, (0 - 0), 0, 1.5]",
         "'수들'", "[[1], [2, [3]]]", "[{\"키\": 1}]", '["b", "a", "B"]', "[5, 4, 3, 2, 1, 0]"]
TEXT_LISTS = ['["가", "나"]', '["a", "b", "c"]', "[]", '[""]', "[1, 2]", '["하나"]']
FUNCS = ["<제곱>", "<짝수인지>", "<더하기>", "<오류내기>", "<숫자돌려주기>", "<출력하고참>",
         "<인자없음>", "<길이>", "<대문자로>", "<끝내기>", "<숫자만>", "<안쪽호출>"]
OTHERS = ["참", "비어있음", '{"키": 1}', "<제곱>"]
# Hari source text of string literals: \" is a quote, \n a line break, \t a tab.
JSON_TEXTS = [r'"{\"a\": 1, \"b\": [1, 2.5, -0, true, null]}"', r'"[1, 2, 3]"', r'"  \"문자\"  "', r'"1e400"',
              r'"1e-400"', r'"-0"', r'"01"', r'"1."', r'".5"', r'"[1,]"', r'"{\"a\":1,\"a\":2}"', r'"\"\ud800\""',
              r'"\"😀x\""', r'"\"\udc00\ud800\""', r'"nul"', r'"[[[[]]]]"', r'"{}"', r'"[]"', r'""',
              r'"12345678901234567890"', r'"0.1e+2"', r'"[1 2]"', r'"{\"k\" 1}"', r'"true false"', r'"\"aé\""',
              r'"{\"x\": {\"y\": [null]}}"', r'"\"탭\tX\""', r'"-"', r'"1e5"', r'"[1e21, 1e-7, 123456789012345680000]"',
              r'"[1, {\"a\": \"b\"}]\n"', r'"\"\u0001\""', r'"{\"\": 0}"', r'"[-1.5E-3]"']
VALUES = [r'{"b": 1, "a": [1, 2]}', r'[1, "둘", 참, 비어있음, 1.5]', r'"따옴표\"와 역슬래시\\"', "(0 - 0)", "(1 / 0)",
          r'{1: 2}', "[1000000000000000000000]", "0.0000001", "123456789012345680000", "[[], {}]", r'"\t"', "<제곱>",
          r'{"k": [{"z": 비어있음}]}', "(0 - 1.25)", "'수들'", r'"\n줄"']
CSV_TEXTS = [r'"a,b\nc,d"', r'"\"x,y\",z\n"', r'"a,\"b\"c\""', r'"\"unterminated"', r'""', r'"\n\n"', r'"a;b;c"',
             r'"x\"y,z"', r'"\"a\"\"b\",c"', "\"﻿a,b\"", r'"a,b\n"', r'",,"', r'"a\tb"', r'"\"\""', r'"줄1\n줄2,셀"']
ROWS = [r'[["a", "b"], ["c", 1]]', r'[[""]]', r'[["x,y", "q\"t"]]', "[[1.5, 비어있음], [(0 - 0)]]", "[[]]", "[1]",
        r'[["줄\n바꿈"]]', "[[참]]", r'[["a", ""], ["", "b"]]', "[]", r'[["1e21", 1000000000000000000000]]',
        "[[(1 / 0)]]", r'[["a;b", "c"]]']
NUM_LISTS = ["[1, 2, 3, 4]", "[]", "[5]", "[2, 8]", "[1, \"a\"]", "[(0 - 0), 0]", "[(0 - 1), (0 - 0)]", "[1.5, 2.5, (1 / 3)]",
             "[1000000000000000000000000, 1]", "[3, 1, 2]", "'수들'", "[0.1, 0.2, 0.3]", "[(0 - 5), 5]", "1", "[[1]]"]
PATHS = ['"a/b/c.txt"', '"/usr/local/"', '"C:\\Users\\me\\x.tar.gz"', '"c:rel/x"', '""', '"."', '".."', '"../../a"',
         '"/../a/./b//c/.."', '".bashrc"', '"a/.hidden.txt"', '"file."', '"x/y/"', '"//"', '"/"', '"C:"', '"C:/"',
         '"한글/경로.하리"', '"a\\b"', '"...x"', '"1:2"', '"https://x.com/a"']
BASE64 = ['"SGVsbG8="', '"SGVsbG8"', '"QQ=="', '"QR=="', '"QUI="', '"QUJD"', '""', '"===="', '"A==="', '"7Jes"',
          '"7J6="', '"gA=="', '"Zm9v YmFy"', r'"Zm9v\nYmFy"', '"QUJDRA=="', '"_-8="', '"QUJD/"', '"AB=C"']
URLS = ['"a%20b"', '"%E1%84%80"', '"%ea%b0%80"', '"%"', '"a%2"', '"%41"', '"x%4"', '"%GG"', '"%FF"', '"+%2B"', '"100%"',
        '"%%41"', '"한글"', '"a~b_c.d-e"']
DELIMS = ['";"', r'"\t"', '","', '"ab"', '""', r'"\""', '"|"', r'"\n"', "1"]

PRELUDE = """<제곱>을 만들자 ('x'):
    ('x' * 'x')를 돌려주자
<짝수인지>를 만들자 ('x'):
    ('x' % 2 == 0)를 돌려주자
<더하기>를 만들자 ('a', 'b'):
    ('a' + 'b')를 돌려주자
<오류내기>를 만들자 ('x'):
    새로운 [오류]("콜백에서")를 발생시키자
<숫자돌려주기>를 만들자 ('x'):
    1을 돌려주자
<출력하고참>을 만들자 ('x'):
    'x'를 출력하자
    참을 돌려주자
<인자없음>을 만들자 ():
    "없음"을 돌려주자
<길이>를 만들자 ([숫자]인 'x'):
    ('x' + 1)을 돌려주자
<대문자로>를 만들자 ('x'):
    <대문자>('x')를 돌려주자
<끝내기>를 만들자 ('x'):
    반복을 끝내자
[숫자]를 돌려주는 <숫자만>을 만들자 ('x'):
    'x'를 돌려주자
<안쪽호출>을 만들자 ('x'):
    <변환하기>([1, 2], <제곱>)을 출력하자
    참을 돌려주자
'수들'을 [4, 1, 3]으로 정하자
"""


class Gen:
    def __init__(self, rng, modules):
        self.r = rng
        self.modules = modules

    def arg(self, kind):
        r = self.r
        if r.random() < 0.08:
            kind = r.choice("snlfav")
        if kind == "s":
            return r.choice(TEXTS)
        if kind in "ic":
            if kind == "c" and r.random() < 0.8:
                return r.choice(['"*"', '"0"', '"-"', '"ab"', '""'])
            return r.choice(NUMS)
        if kind == "n":
            return r.choice(NUMS)
        if kind == "l":
            return r.choice(LISTS)
        if kind == "L":
            return r.choice(TEXT_LISTS)
        if kind == "j":
            return r.choice(JSON_TEXTS)
        if kind == "v":
            return r.choice(VALUES + LISTS)
        if kind == "C":
            return r.choice(CSV_TEXTS)
        if kind == "R":
            return r.choice(ROWS)
        if kind == "d":
            return r.choice(DELIMS)
        if kind == "N":
            return r.choice(NUM_LISTS)
        if kind == "P":
            return r.choice(PATHS)
        if kind == "B":
            return r.choice(BASE64)
        if kind == "U":
            return r.choice(URLS)
        if kind == "D":
            return r.choice(SECONDS)
        if kind == "F":
            return r.choice(LAYOUTS)
        if kind == "Y":
            return r.choice(DATE_TEXTS)
        if kind == "Z":
            return r.choice(ZONES)
        if kind == "X":
            return '"' + self.pattern() + '"'
        if kind == "T":
            return r.choice(REGEX_TEXTS)
        if kind == "E":
            return r.choice(REPLACEMENTS)
        if kind == "f":
            return r.choice(FUNCS)
        return r.choice(TEXTS + NUMS + LISTS + OTHERS)

    def pattern(self, depth=0):
        r = self.r
        out = []
        for _ in range(r.randrange(1, 4)):
            k = r.random()
            if k < 0.12 and depth < 3:
                inner = self.pattern(depth + 1)
                head = r.choice(["(", "(?:", "(?i:", "(?P<n>", "(?<g1>", "(?s:", "(?m:", "(?U:", "(?i-s:", "(?-i:",
                                 "(?<>", "(?x:", "(?-:", "(?'n'", "(?i)(", "(?P=n>"])
                piece = head + inner + ("|" + self.pattern(depth + 1) if r.random() < 0.3 else "") + ")"
                if r.random() < 0.03:
                    piece = piece[:-1]
            elif k < 0.16:
                piece = r.choice(["(?i)", "(?s)", "(?m)", "(?U)", "(?-m)", "(?i-i)", "(?)", "|", ")"])
            else:
                piece = r.choice(RX_ATOMS)
            if r.random() < 0.3:
                piece += r.choice(RX_QUANT)
            out.append(piece)
        return "".join(out)

    def call(self):
        r = self.r
        mod = r.choice(self.modules)
        name, spec = r.choice(MODULES[mod])
        if spec == "!":
            return r.choice(SPECIAL_CALLS[mod])
        kinds = []
        i = 0
        while i < len(spec):
            k = spec[i]
            optional = i + 1 < len(spec) and spec[i + 1] == "?"
            if not optional or r.random() < 0.5:
                kinds.append(k)
            i += 2 if optional else 1
        if r.random() < 0.05:
            kinds = kinds[:-1] if kinds and r.random() < 0.5 else kinds + ["a"]
        return f"<{PREFIX.get(mod, '')}{name}>({', '.join(self.arg(k) for k in kinds)})"

    def stmt(self):
        r = self.r
        c = self.call()
        k = r.random()
        if k < 0.04:
            return f"{c}를 출력하자"
        if k < 0.1:
            return f"1부터 3까지 반복하자 ('i'):\n    'i'를 출력하자\n    일단 해보자:\n        {c}를 출력하자\n    오류가 발생했다면 ('e'):\n        ('e'의 '메시지')를 출력하자"
        catch = "'e'를 출력하자" if r.random() < 0.3 else "('e'의 '메시지')를 출력하자"
        return f"일단 해보자:\n    {c}를 출력하자\n오류가 발생했다면 ('e'):\n    {catch}"

    def program(self):
        mods = self.modules
        imports = []
        for m in mods:
            for name, _ in MODULES[m]:
                if m in PREFIX:
                    imports.append(f"[{m}]에서 <{name}>을 <{PREFIX[m]}{name}>으로 가져오자")
                else:
                    imports.append(f"[{m}]에서 <{name}>을 가져오자")
        if "텍스트" not in mods:
            imports.append("[텍스트]에서 <대문자>를 가져오자")
        if "목록" not in mods:
            imports.append("[목록]에서 <변환하기>를 가져오자")
        body = [self.stmt() for _ in range(self.r.randrange(10, 25))]
        return "\n".join(imports) + "\n" + PRELUDE + "\n".join(body) + "\n'수들'을 출력하자\n"


def main():
    out, count = sys.argv[1], int(sys.argv[2])
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    mods = sys.argv[4].split(",") if len(sys.argv) > 4 else list(MODULES)
    os.makedirs(out, exist_ok=True)
    for i in range(count):
        gen = Gen(random.Random(seed * 100003 + i), mods)
        with open(os.path.join(out, f"s{i:05d}.hr"), "w", encoding="utf-8", newline="\n") as f:
            f.write(gen.program())


if __name__ == "__main__":
    main()
