"""Writes random Hari programs for differential testing (tools/runcheck.sh).

    python tools/fuzz/gen.py <out dir> <count> [seed] [chaos]

Programs are type-aware so that most run to the end: number variables stay
numbers, text stays text. With probability `chaos` (default 0.015) a choice is
replaced by something wrong (a type mismatch, a missing variable, a bad index),
so error paths are compared too. Loops are small so every program finishes.
Everything stays within what Haru runs so far.
"""
import os
import random
import sys

NUMS = ["'가'", "'나'", "'다'"]
TEXT = "'글'"
NUMLIST = "'수들'"
MIXLIST = "'섞음'"
DICT = "'사전'"
TYPES_OK = {"num": ["[숫자]", "[아무거나]", ""], "str": ["[문자열]", ""], "list": ["[(숫자)목록]", "[목록]", ""]}
WRONG_TYPES = ["[문자열]", "[논리]", "[(문자열)목록]", "[사전]"]


class Gen:
    def __init__(self, rng, chaos):
        self.r = rng
        self.chaos = chaos
        self.depth = 0
        self.loops = 0
        self.in_func = False
        self.loopvars = []

    def pick(self, xs):
        return self.r.choice(xs)

    def chance(self, p):
        return self.r.random() < p

    def bad(self):
        return self.chance(self.chaos)

    # ---- expressions by type

    def num(self, d=2):
        if self.bad():
            return self.pick(["'없는변수'", '"글자"', "참", "비어있음", NUMLIST, "(" + TEXT + " + 1)"])
        if d <= 0 or self.chance(0.35):
            k = self.r.randrange(9)
            if k < 3:
                return str(self.pick([0, 1, 2, 3, 5, 7, 10, 12, 100, 0.5, 2.5, 1000000]))
            if k < 6:
                return self.pick(NUMS + self.loopvars)
            if k == 6:
                return f"({NUMLIST}의 '길이')"
            if k == 7:
                return f"({self.pick([NUMLIST])}의 {self.r.randrange(1, 3)}번째)"
            return f"({TEXT}의 '길이')"
        k = self.r.randrange(10)
        if k < 6:
            op = self.pick(["+", "-", "*", "+", "-"])
            return f"({self.num(d - 1)} {op} {self.num(d - 1)})"
        if k == 6:
            return f"({self.num(d - 1)} {self.pick(['/', '%'])} {self.pick(['2', '3', '7', '0.5'])})"
        if k == 7 and not self.in_func:
            return self.call_num(d - 1)
        if k == 8:
            return "<숫자로>(" + self.pick(['"12"', '"3.5"', '"-2"', '"1_000"']) + ")"
        return "<코드로>(" + self.pick(['"가"', '"a"', '"하"']) + ")"

    def text(self, d=2):
        if self.bad():
            return self.pick(["'없는변수'", "3", "[1]", f"({TEXT} - 1)"])
        if d <= 0 or self.chance(0.35):
            k = self.r.randrange(6)
            if k < 3:
                return '"' + self.pick(["안녕", "하리", "", "a b", "줄\\n바꿈", "따옴\\\"표", "탭\\t", "가나다"]) + '"'
            if k < 5:
                return TEXT
            return f"({TEXT}의 {self.r.randrange(1, 3)}번째)"
        k = self.r.randrange(8)
        if k < 3:
            return f"({self.text(d - 1)} + {self.text(d - 1)})"
        if k == 3:
            return f"<문자로>({self.pick([self.num(d - 1), self.cond(0), NUMLIST, DICT, '비어있음'])})"
        if k == 4:
            return f"<글자로>({self.pick(['44032', '65', '97', '54616'])})"
        if k == 5:
            return f'틀"{self.pick(["값 ", "", "합: "])}{{{self.pick(NUMS + [TEXT, NUMLIST])}}}{self.pick(["", " 끝", " {"])}"'
        if k == 6:
            return TEXT + "의 " + self.pick(['<자르기>(1, 2)', '<자르기>(2, 10)', '<바꾸기>("안", "반")'])
        return "(" + TEXT + "의 " + self.pick(['<분리하기>(" ")', '<분리하기>("")']) + "의 1번째)"

    def cond(self, d=1):
        if self.bad():
            return self.pick(["1", '"참"', "'없는변수'", "비어있음"])
        k = self.r.randrange(7)
        if k < 3:
            return f"({self.num(d)} {self.pick(['>', '<', '>=', '<=', '==', '!='])} {self.num(d)})"
        if k == 3:
            return f"({self.pick(NUMS)}가 {self.num(0)}{self.pick(['와 같다', '보다 크다', '보다 작다', '와 다르다'])})"
        if k == 4 and d > 0:
            return f"({self.cond(d - 1)} {self.pick(['그리고', '또는'])} {self.cond(d - 1)})"
        if k == 5:
            return f"({TEXT}의 <포함확인>({self.text(0)}))"
        return f"({self.text(0)} == {self.text(0)})"

    def any(self, d=2):
        k = self.r.randrange(6)
        if k < 2:
            return self.num(d)
        if k < 4:
            return self.text(d)
        if k == 4:
            return self.cond(1)
        return "[" + ", ".join(self.num(0) for _ in range(self.r.randrange(0, 3))) + "]"

    def call_num(self, d):
        f = self.pick(["<두배>", "<합>", "<세기>"] + (["<없는함수>"] if self.bad() else []))
        if f == "<두배>":
            return f"<두배>({self.num(d)})"
        if f == "<합>":
            args = [self.num(d)] if self.chance(0.4) else [self.num(d), self.num(d)]
            if self.bad():
                args.append("1")
            return f"<합>({', '.join(args)})"
        return f"{f}({self.num(d)})"

    # ---- statements

    def block(self, ind, n=None):
        self.depth += 1
        out = [self.stmt(ind) for _ in range(n or self.r.randrange(1, 4))]
        self.depth -= 1
        return "\n".join(out)

    def stmt(self, ind):
        pad = "    " * ind
        deep = self.depth < 3
        k = self.r.randrange(24)
        if k < 4:
            v = self.pick(NUMS)
            t = self.pick(TYPES_OK["num"])
            if self.bad():
                t = self.pick(WRONG_TYPES)
            t = f"{t}인 " if t else ""
            verb = "고정하자" if self.chance(0.03) else "정하자"
            return f"{pad}{v}를 {t}{self.num()}로 {verb}"
        if k == 4:
            return f"{pad}{TEXT}를 {self.text()}로 정하자"
        if k < 8:
            verb = "이어출력하자" if self.chance(0.1) else "출력하자"
            return f"{pad}{self.any()}를 {verb}"
        if k < 10:
            v = self.pick(NUMS)
            return f"{pad}{v}에 {self.num(1)}를 {self.pick(['더하자', '빼자'])}"
        if k == 10:
            return f"{pad}{TEXT}에 {self.text(0)}를 더하자"
        if k == 11 and deep:
            s = f"{pad}만약 {self.cond()} 라면:\n{self.block(ind + 1)}"
            r = self.r.randrange(3)
            if r == 0:
                s += f"\n{pad}그렇지 않다면:\n{self.block(ind + 1)}"
            elif r == 1:
                s += f"\n{pad}그렇지 않고 만약 {self.cond()} 라면:\n{self.block(ind + 1)}"
            return s
        if k == 12 and deep:
            a, b = self.r.randrange(0, 4), self.r.randrange(0, 5)
            var = self.pick(["'i'", "'j'"])
            self.loops += 1
            self.loopvars.append(var)
            s = f"{pad}{a}부터 {b}까지 반복하자 ({var}):\n{self.block(ind + 1)}"
            self.loopvars.pop()
            self.loops -= 1
            return s
        if k == 13 and deep:
            self.loops += 1
            self.loopvars.append("'항목'")
            s = f"{pad}{self.pick([NUMLIST, NUMLIST, TEXT])}의 '항목'마다 반복하자:\n{self.block(ind + 1)}"
            self.loopvars.pop()
            self.loops -= 1
            return s
        if k == 14 and deep:
            self.loops += 1
            s = f"{pad}'횟수'를 0으로 정하자\n{pad}('횟수' < 3) 동안 반복하자:\n{pad}    '횟수'에 1을 더하자\n{self.block(ind + 1)}"
            self.loops -= 1
            return s
        if k == 15 and (self.loops or self.bad()):
            return f"{pad}반복을 끝내자"
        if k == 16:
            pos = self.pick(["", " 앞에", " 뒤에"])
            val = self.num(1) if not self.bad() else '"글자"'
            return f"{pad}{NUMLIST}{pos}에 {val}를 추가하자"
        if k == 17:
            pos = self.pick(["", " 앞에서", " 뒤에서"])
            if self.chance(0.5):
                return f"{pad}{NUMLIST}{pos}에서 꺼내자"
            return f"{pad}({NUMLIST} {self.pick(['앞에서', '뒤에서'])} 꺼낸 값)을 출력하자"
        if k == 18:
            if self.chance(0.5):
                return f"{pad}{NUMLIST}의 {self.r.randrange(1, 5)}번째를 {self.num(1)}로 정하자"
            key = self.pick(['"키"', '"새키"', '1'])
            return f"{pad}{DICT}의 {key}를 {self.any(1)}로 정하자"
        if k == 19:
            key = self.pick(['"키"', '"값"', '"새키"'])
            return f"{pad}({DICT}의 {key})를 출력하자"
        if k == 20 and deep:
            cases = []
            for _ in range(self.r.randrange(1, 3)):
                vals = ", ".join(str(self.r.randrange(0, 5)) for _ in range(self.r.randrange(1, 3)))
                body = self.block(ind + 2, 1)
                if self.chance(0.3):
                    body += f"\n{pad}        다음으로 이어가자"
                cases.append(f"{pad}    {vals} 인 경우:\n{body}")
            if self.chance(0.6):
                cases.append(f"{pad}    나머지는:\n{self.block(ind + 2, 1)}")
            return f"{pad}{self.pick(NUMS)}에 따라 나누자:\n" + "\n".join(cases)
        if k == 21 and not self.in_func:
            return f"{pad}{self.call_num(1)}를 실행하자"
        if k == 22 and self.in_func:
            return f"{pad}{self.num(1)}를 돌려주자"
        if k == 23 and self.chance(0.1):
            return f"{pad}{NUMLIST}의 <비우기>()를 실행하자"
        return f"{pad}{self.any()}를 출력하자"

    def functions(self):
        self.in_func = True
        out = [
            f"[숫자]를 돌려주는 <두배>를 만들자 ([숫자]인 'x'):\n{self.fbody()}\n    ('x' * 2)를 돌려주자",
            f"<합>을 만들자 ('x', 'y' = 10):\n{self.fbody()}\n    ('x' + 'y')를 돌려주자",
            f"<세기>를 만들자 ([숫자]인 'n'):\n    '개수'를 0으로 정하자\n    1부터 'n'까지 반복하자 ('k'):\n        '개수'에 'k'를 더하자\n        만약 ('k' > 5) 라면:\n            반복을 끝내자\n    '개수'를 돌려주자",
        ]
        self.in_func = False
        return out

    def fbody(self):
        saved = self.loopvars
        self.loopvars = ["'x'"]
        b = self.block(1, self.r.randrange(0, 3) or 1)
        self.loopvars = saved
        return b

    def program(self):
        out = self.functions()
        out.append(f"{NUMLIST}를 [1, 2, 3]으로 정하자")
        out.append(f"{MIXLIST}를 [\"가\", 참, 3]으로 정하자")
        out.append(f"{DICT}를 {{\"키\": 1, \"값\": \"둘\"}}로 정하자")
        out.append(f"{TEXT}를 \"안녕 하리\"로 정하자")
        for v in NUMS:
            out.append(f"{v}를 {self.r.randrange(0, 10)}로 정하자")
        for _ in range(self.r.randrange(8, 20)):
            out.append(self.stmt(0))
        out.append(f"{NUMS[0]}를 출력하자\n{TEXT}를 출력하자\n{NUMLIST}를 출력하자\n{DICT}를 출력하자")
        return "\n".join(out) + "\n"


CLASSES = """[동물]을 설계하자:
    '이름'을 [문자열]인 "없음"으로 정하자
    '나이'를 [숫자]인 {age}으로 정하여 숨기자
    '다리'를 4로 정하여 물려주자
    '우리'의 '수'를 0으로 정하자
    '별명'을 [문자열]로 정하자:
        가져올 때:
            ('나'의 '이름' + "이")를 돌려주자
        정할 때 ('새값'):
            '나'의 '이름'을 '새값'으로 정하자
    처음 만들어질 때 ('새이름', '새나이' = 1) 다음과 같이 하자:
        '나'의 '이름'을 '새이름'으로 정하자
        '나이'를 '새나이'로 정하자
        [동물]의 '수'에 1을 더하자
    [문자열]을 돌려주는 <소개>를 만들자 ():
        ('나'의 '이름' + "입니다")를 돌려주자
    <나이먹기>를 만들자 ([숫자]인 '더'):
        '나이'에 '더'를 더하자
        '나이'를 돌려주자
    <다리수>를 만들자 ():
        '나'의 '다리'를 돌려주자
    <기호 같다>를 만들자 ('다른'):
        ('나'의 '이름' == '다른'의 '이름')를 돌려주자
    [동물]을 돌려주는 '우리'의 <만들기>를 만들자 ('이름'):
        새로운 [동물]('이름')을 돌려주자
    <비밀>을 만들어 숨기자 ():
        "비밀"을 돌려주자

[개]는 [동물]을 바탕으로 하고 설계하자:
    처음 만들어질 때 ('새이름') 다음과 같이 하자:
        부모의 <처음 만들어질 때>('새이름', 3)을 실행하자
    <소개>를 만들자 ():
        (부모의 <소개>() + " 멍")을 돌려주자

[내오류]는 [오류]를 바탕으로 하고 설계하자:
    '코드'를 7로 정하자

'동'을 [동물]인 새로운 [동물]("가나", 2)로 정하자
'멍'을 새로운 [개]("바둑")로 정하자
"""


class OopGen(Gen):
    """Programs with classes and error handling on top of Gen's statements."""

    def obj(self):
        return self.pick(["'동'", "'멍'", "새로운 [동물](\"새\")", "[동물]의 <만들기>(\"공장\")"] + (["'없는변수'", "3"] if self.bad() else []))

    def stmt(self, ind):
        pad = "    " * ind
        k = self.r.randrange(30)
        deep = self.depth < 3
        if k < 16:
            return Gen.stmt(self, ind)
        if k == 16:
            m = self.pick(["<소개>()", "<나이먹기>(1)", "<다리수>()", "<나이먹기>(\"한살\")", "<없는메서드>()"] + (["<비밀>()"] if self.bad() else []))
            return f"{pad}({self.obj()}의 {m})를 출력하자"
        if k == 17:
            f = self.pick(["'이름'", "'별명'", "'다리'", "'없는필드'"] + (["'나이'"] if self.bad() else []))
            return f"{pad}({self.obj()}의 {f})를 출력하자"
        if k == 18:
            v = self.pick(['"새이름"', "3"])
            field = self.pick(["'이름'", "'별명'", "'새필드'"])
            return f"{pad}'동'의 {field}를 {v}로 정하자"
        if k == 19:
            key = self.pick(['"키"', '"값"', '"새키"'])
            return f"{pad}({DICT}의 {key})를 출력하자"
        if k == 20 and deep:
            cases = []
            for _ in range(self.r.randrange(1, 3)):
                vals = ", ".join(str(self.r.randrange(0, 5)) for _ in range(self.r.randrange(1, 3)))
                body = self.block(ind + 2, 1)
                if self.chance(0.3):
                    body += f"\n{pad}        다음으로 이어가자"
                cases.append(f"{pad}    {vals} 인 경우:\n{body}")
            if self.chance(0.6):
                cases.append(f"{pad}    나머지는:\n{self.block(ind + 2, 1)}")
            return f"{pad}{self.pick(NUMS)}에 따라 나누자:\n" + "\n".join(cases)
        if k == 21 and not self.in_func:
            return f"{pad}{self.call_num(1)}를 실행하자"
        if k == 22 and self.in_func:
            return f"{pad}{self.num(1)}를 돌려주자"
        if k == 23 and self.chance(0.1):
            return f"{pad}{NUMLIST}의 <비우기>()를 실행하자"
        return f"{pad}{self.any()}를 출력하자"

    def functions(self):
        self.in_func = True
        out = [
            f"[숫자]를 돌려주는 <두배>를 만들자 ([숫자]인 'x'):\n{self.fbody()}\n    ('x' * 2)를 돌려주자",
            f"<합>을 만들자 ('x', 'y' = 10):\n{self.fbody()}\n    ('x' + 'y')를 돌려주자",
            f"<세기>를 만들자 ([숫자]인 'n'):\n    '개수'를 0으로 정하자\n    1부터 'n'까지 반복하자 ('k'):\n        '개수'에 'k'를 더하자\n        만약 ('k' > 5) 라면:\n            반복을 끝내자\n    '개수'를 돌려주자",
        ]
        self.in_func = False
        return out

    def fbody(self):
        saved = self.loopvars
        self.loopvars = ["'x'"]
        b = self.block(1, self.r.randrange(0, 3) or 1)
        self.loopvars = saved
        return b

    def program(self):
        out = self.functions()
        out.append(f"{NUMLIST}를 [1, 2, 3]으로 정하자")
        out.append(f"{MIXLIST}를 [\"가\", 참, 3]으로 정하자")
        out.append(f"{DICT}를 {{\"키\": 1, \"값\": \"둘\"}}로 정하자")
        out.append(f"{TEXT}를 \"안녕 하리\"로 정하자")
        for v in NUMS:
            out.append(f"{v}를 {self.r.randrange(0, 10)}로 정하자")
        for _ in range(self.r.randrange(8, 20)):
            out.append(self.stmt(0))
        out.append(f"{NUMS[0]}를 출력하자\n{TEXT}를 출력하자\n{NUMLIST}를 출력하자\n{DICT}를 출력하자")
        return "\n".join(out) + "\n"


CLASSES = """[동물]을 설계하자:
    '이름'을 [문자열]인 "없음"으로 정하자
    '나이'를 [숫자]인 {age}으로 정하여 숨기자
    '다리'를 4로 정하여 물려주자
    '우리'의 '수'를 0으로 정하자
    '별명'을 [문자열]로 정하자:
        가져올 때:
            ('나'의 '이름' + "이")를 돌려주자
        정할 때 ('새값'):
            '나'의 '이름'을 '새값'으로 정하자
    처음 만들어질 때 ('새이름', '새나이' = 1) 다음과 같이 하자:
        '나'의 '이름'을 '새이름'으로 정하자
        '나이'를 '새나이'로 정하자
        [동물]의 '수'에 1을 더하자
    [문자열]을 돌려주는 <소개>를 만들자 ():
        ('나'의 '이름' + "입니다")를 돌려주자
    <나이먹기>를 만들자 ([숫자]인 '더'):
        '나이'에 '더'를 더하자
        '나이'를 돌려주자
    <다리수>를 만들자 ():
        '나'의 '다리'를 돌려주자
    <기호 같다>를 만들자 ('다른'):
        ('나'의 '이름' == '다른'의 '이름')를 돌려주자
    [동물]을 돌려주는 '우리'의 <만들기>를 만들자 ('이름'):
        새로운 [동물]('이름')을 돌려주자
    <비밀>을 만들어 숨기자 ():
        "비밀"을 돌려주자

[개]는 [동물]을 바탕으로 하고 설계하자:
    처음 만들어질 때 ('새이름') 다음과 같이 하자:
        부모의 <처음 만들어질 때>('새이름', 3)을 실행하자
    <소개>를 만들자 ():
        (부모의 <소개>() + " 멍")을 돌려주자

[내오류]는 [오류]를 바탕으로 하고 설계하자:
    '코드'를 7로 정하자

'동'을 [동물]인 새로운 [동물]("가나", 2)로 정하자
'멍'을 새로운 [개]("바둑")로 정하자
"""


class OopGen(Gen):
    """Programs with classes and error handling on top of Gen's statements."""

    def obj(self):
        return self.pick(["'동'", "'멍'", "새로운 [동물](\"새\")", "[동물]의 <만들기>(\"공장\")"] + (["'없는변수'", "3"] if self.bad() else []))

    def stmt(self, ind):
        pad = "    " * ind
        k = self.r.randrange(30)
        deep = self.depth < 3
        if k < 16:
            return Gen.stmt(self, ind)
        if k == 16:
            m = self.pick(["<소개>()", "<나이먹기>(1)", "<다리수>()", "<나이먹기>(\"한살\")", "<없는메서드>()"] + (["<비밀>()"] if self.bad() else []))
            return f"{pad}({self.obj()}의 {m})를 출력하자"
        if k == 17:
            f = self.pick(["'이름'", "'별명'", "'다리'", "'없는필드'"] + (["'나이'"] if self.bad() else []))
            return f"{pad}({self.obj()}의 {f})를 출력하자"
        if k == 18:
            v = self.pick(['"새이름"', "3"])
            return f"{pad}'가'의 {self.pick(['이름', '별명', '새필드'])}를 {v}로 정하자".replace("의 이", "의 '이").replace("의 별", "의 '별").replace("의 새", "의 '새").replace("를 ", "'를 ", 1)
        if k == 19:
            return f"{pad}([동물]의 '수')를 출력하자"
        if k == 20:
            return f"{pad}({self.obj()} == {self.obj()})를 출력하자"
        if k == 21:
            return f"{pad}({self.obj()}가 {self.pick(['[동물]', '[개]', '[오류]'])}의 일종이다)를 출력하자"
        if k == 22:
            return f"{pad}{self.obj()}를 출력하자"
        if k in (23, 24, 25) and deep:
            what = self.pick([
                "새로운 [오류](\"문제\")를 발생시키자",
                "새로운 [내오류](\"내 문제\")를 발생시키자",
                "\"글자\"를 발생시키자",
                "(1 / 0)을 출력하자",
                "'없는변수'를 출력하자",
                "\"괜찮아\"를 출력하자",
            ])
            body = f"{pad}    {what}\n" + self.block(ind + 1, 1)
            s = f"{pad}일단 해보자:\n{body}"
            for _ in range(self.r.randrange(0, 3)):
                t = self.pick(["오류가", "[내오류]가", "[오류]가", "[동물]이"])
                s += f"\n{pad}{t} 발생했다면 ('e'):\n{pad}    ('e'의 '메시지')를 출력하자"
                if self.chance(0.3):
                    s += f"\n{pad}    'e'를 출력하자"
            if self.chance(0.5) or "발생했다면" not in s:
                s += f"\n{pad}마무리는 항상:\n{pad}    \"마무리\"를 출력하자"
            return s
        if k == 26:
            if self.bad():
                thrown = self.pick(['새로운 [오류]("밖")', '"밖에서"', '3'])
                return f"{pad}{thrown}을 발생시키자"
            return f"{pad}\"조용\"을 출력하자"
        if k == 27 and deep:
            self.loops += 1
            s = f"{pad}1부터 3까지 반복하자 ('i'):\n{pad}    일단 해보자:\n{pad}        만약 ('i' == 2) 라면:\n{pad}            반복을 끝내자\n{pad}        'i'를 출력하자\n{pad}    마무리는 항상:\n{pad}        \"끝\"을 출력하자"
            self.loops -= 1
            return s
        return f"{pad}({self.obj()}의 <소개>())를 출력하자"

    def program(self):
        base = Gen.program(self)
        head = CLASSES.replace("{age}", str(self.r.randrange(0, 5)))
        body = [self.stmt(0) for _ in range(self.r.randrange(6, 14))]
        return head + base + "\n".join(body) + "\n"


def main():
    args = [a for a in sys.argv[1:] if a != "--oop"]
    oop = "--oop" in sys.argv
    out, count = args[0], int(args[1])
    seed = int(args[2]) if len(args) > 2 else 1
    chaos = float(args[3]) if len(args) > 3 else 0.015
    os.makedirs(out, exist_ok=True)
    for i in range(count):
        rng = random.Random(seed * 100003 + i)
        gen = OopGen(rng, chaos) if oop else Gen(rng, chaos)
        with open(os.path.join(out, f"f{i:05d}.hr"), "w", encoding="utf-8", newline="\n") as f:
            f.write(gen.program())


if __name__ == "__main__":
    main()
