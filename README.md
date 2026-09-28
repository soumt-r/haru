<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/banner-dark.svg">
    <img alt="haru" src="assets/banner-light.svg">
  </picture>
</h1>

<p align="center">
  <a href="README.md">한국어</a> | <a href="README.ja.md">日本語</a>
</p>

<p align="center">
  <a href="https://github.com/soumt-r/haru/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/soumt-r/haru/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-stable-b69cf6?logo=rust&logoColor=white">
  <img alt="JIT: Cranelift" src="https://img.shields.io/badge/JIT-Cranelift-a98bf2">
  <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-b69cf6"></a>
</p>

<p align="center">
  <b>Haru</b> runs <b>Hari</b> (하리) and <b>Kanade</b> (カナデ),<br>
  the programming languages you write in Korean and Japanese, in Rust.<br>
  <sub>이름 <b>하루</b>는 봄을 뜻하는 春(はる)과, 우리말 하루(day)를 함께 뜻해요.</sub>
</p>

<br>

<table>
<tr>
<th align="center">하리 (Hari) · 한국어</th>
<th align="center">カナデ (Kanade) · 日本語</th>
</tr>
<tr>
<td valign="top">

```hari
<인사>를 만들자 ([문자열]인 '이름'):
    틀"안녕, {'이름'}!"을 출력하자

'이름들'을 [(문자열)목록]인 ["하리", "카나데"]로 정하자
'이름들'의 '이름'마다 반복하자:
    <인사>('이름')을 실행하자
```

</td>
<td valign="top">

```kanade
〈挨拶〉を作ろう(【文字列】の『名前』):
    枠「こんにちは、{『名前』}！」を出力しよう

『名前たち』を【(文字列)リスト】の【「ハリ」,「カナデ」】にしよう
『名前たち』の『名前』ごとに繰り返そう:
    〈挨拶〉(『名前』)を実行しよう
```

</td>
</tr>
</table>

<br>

## 하나와 하루

하리와 카나데의 기준 런타임은 Go로 만든 [**하나**](https://github.com/soumt-r/hana)예요. 하루는 같은 언어를 Rust로 처음부터 다시 만든 런타임이에요.

- **언어는 똑같아요.** 같은 프로그램은 하나와 똑같이 출력하고, 오류 문구까지 같아요. 수천 개의 프로그램을 두 런타임에서 돌려 결과를 맞춰 보며 만들고 있어요.
- **실행 구조는 새로 짰어요.** 16바이트 값, 레지스터 VM, 그리고 기본으로 켜져 있는 [Cranelift](https://cranelift.dev) JIT로 더 빠르게 돌아요.
- **패키지를 Rust로 쓸 수 있어요.** Rust 함수를 그대로 하리·카나데 함수로 내보내고, 프로그램과 함께 실행 파일 하나로 묶을 수 있어요.

설계와 그 이유는 [DESIGN.md](DESIGN.md)에 자세히 적어 두었어요.

## 빠른 시작

[Rust](https://rustup.rs) (stable)가 필요해요.

```bash
cargo build --release                         # target/release/haru 가 만들어져요
./target/release/haru run hello.hr            # 하리 프로그램 실행 (.knd 는 카나데)
./target/release/haru run --no-jit hello.hr   # JIT 없이 인터프리터로만 실행
```

`--time`을 붙이면 파싱·컴파일과 실행에 걸린 시간을 보여줘요.

## 명령

| 명령 | 하는 일 |
| --- | --- |
| `haru run <파일>` | `.hr`(하리)나 `.knd`(카나데) 프로그램을 실행해요. 자주 도는 함수는 기계어로 바꿔 가며 달려요 |
| `haru run --no-jit <파일>` | JIT 없이 인터프리터로만 실행해요 |
| `haru build [--with <패키지>]... [-o <파일>] [<프로그램>]` | 패키지(와 프로그램)를 실행 파일 하나로 묶어요 |
| `haru init` | 이 폴더에 `haru.toml`을 만들어요 |
| `haru add <git 경로>[@버전]` | 패키지를 추가하고 `haru.toml`, `haru.lock`에 적어요 |
| `haru install` · `haru remove <git 경로>` · `haru list` | 패키지를 내려받고, 빼고, 보여줘요 |
| `haru modules [--lang kanade]` | 쓸 수 있는 표준 모듈과 함수 이름을 보여줘요 |
| `haru call <모듈> <함수> [인자...]` | 모듈 함수를 바로 불러 봐요 (`haru call 수학 올림 3.2`) |
| `haru dis <파일>` | 컴파일한 레지스터 코드를 보여줘요 |
| `haru version` | 버전을 보여줘요 |

`--allow-file=false`와 `--allow-net=false`는 하나처럼 `[파일]`, `[소켓]`, `[HTTP]` 모듈을 막아요.

## 얼마나 빠른가요?

같은 컴퓨터에서 프로그램 안에서 잰 실행 시간이에요 (여러 번 잰 것 중 가장 빠른 값, 밀리초).

| 프로그램 | 하나 | 하루 `--no-jit` | 하루 (기본, JIT) |
| --- | ---: | ---: | ---: |
| 피보나치 재귀 (`fib(30)`) | 154 | 127 | **35** |
| 숫자 반복문 | 96 | 48 | **11** |
| 산술 | 223 | 68 | **10** |
| 클래스와 메서드 | 56 | 26 | **20** |
| 아희 해석기로 99병 노래 | 172 | 137 | **84** |
| 브레인퍽 해석기 (약 200만 걸음) | 511 | 376 | **103** |

프로그램을 시작해서 끝낼 때까지 걸리는 전체 시간도 짧아요. 작은 예제 하나를 돌리는 데 하나는 0.4초쯤, 하루는 0.07초쯤 걸려요.

<details>
<summary><b>JIT는 이렇게 동작해요</b></summary>

<br>

- 처음에는 모든 코드를 인터프리터가 실행해요. 1000번쯤 불리거나 돌았던 함수만 다른 스레드에서 기계어로 컴파일해서, 준비되면 그때부터 바꿔 써요.
- 숫자만 다루는 반복문은 값을 레지스터에 둔 채로 돌고, 컴파일된 함수끼리는 VM 프레임 없이 바로 서로를 불러요.
- 컴파일된 코드가 모르는 경우를 만나면 그 명령 하나만 인터프리터에 맡기고 다시 이어서 달려요. 그래서 JIT를 켜도 결과는 언제나 같아요.
- 컴파일은 IR을 만드는 일까지 모두 다른 스레드에서 해서, 아주 짧게 끝나는 프로그램도 JIT 때문에 느려지지 않아요 (컴파일이 끝나기 전에 끝나면 그냥 인터프리터로 다 돈 거예요).

JIT는 기본으로 켜져 있어요. `--no-jit`이나 환경 변수 `HARU_JIT=0`으로 끌 수 있고, `HARU_JIT_HOT=<횟수>`는 컴파일을 시작하는 기준이에요 (0이면 처음부터 컴파일해요).

</details>

## 패키지

패키지는 `haru.toml`이 있는 폴더예요. 하리·카나데 소스로 쓸 수도 있고, [`crates/sdk`](crates/sdk)로 쓴 Rust 네이티브 모듈을 함께 둘 수도 있어요.

```rust
use haru_sdk::prelude::*;

fn build(m: &mut Module) {
    m.name("hari", "인사").name("kanade", "挨拶");
    m.func("hello", |who: Str| format!("안녕, {who}!"))
        .name("hari", "인사말")
        .name("kanade", "挨拶文");
}
```

같은 코드가 동적 라이브러리(`.dll`/`.so`/`.dylib`)로도, `haru build --with`로 실행 파일 안에 정적으로 링크되어서도 동작해요. 예제는 [`examples/greet`](examples/greet)에 있어요.

| 패키지 | 하는 일 |
| --- | --- |
| [`haneul`](packages/haneul) | 하늘: Flask처럼 쓰는 웹 프레임워크 (라우팅, 템플릿, 세션) |
| [`http_server`](packages/http_server) | HTTP 서버 |
| [`timezone`](packages/timezone) | IANA 시간대로 시각 서식·읽기·요일·오프셋 |

외부 패키지는 하나처럼 git 경로가 이름이에요. `haru add github.com/주인/저장소`로 추가하고 `[github.com/주인/저장소]에서 <함수>를 가져오자`로 써요.

## 저장소 안내

<details>
<summary><b>폴더 구성</b></summary>

<br>

| 폴더 | 내용 |
| --- | --- |
| [`crates/syntax`](crates/syntax) | 하리·카나데 렉서와 파서 (하나와 같은 트리를 만들어요) |
| [`crates/core`](crates/core) | 값, 컴파일러, 레지스터 VM, JIT([`src/vm/jit.rs`](crates/core/src/vm/jit.rs)), 모듈 레지스트리 |
| [`crates/abi`](crates/abi), [`crates/sdk`](crates/sdk) | 네이티브 모듈 ABI와 Rust로 모듈을 쓰는 SDK |
| [`crates/cli`](crates/cli) | `haru` 명령 |
| [`crates/http`](crates/http) | 서버 패키지(http_server, 하늘)가 함께 쓰는 HTTP/1.1 연결 처리 |
| [`std/`](std) | SDK로 쓴 표준 모듈 (실행 파일에 정적으로 링크돼요) |
| [`packages/`](packages), [`examples/`](examples) | 함께 두는 패키지와 예제 패키지 |
| [`tools/`](tools) | 하나와 비교하는 도구들 (`runcheck.sh`, `jitcheck.sh`, `parity.sh`, 오류 문구·표준 이름표 생성기) |

</details>

<details>
<summary><b>테스트와 하나와의 비교</b></summary>

<br>

```bash
cargo test
tools/runcheck.sh <hana> <haru> <프로그램 폴더>    # 하나와 하루의 출력 비교
tools/jitcheck.sh <haru> <프로그램 폴더>           # JIT를 켰을 때와 껐을 때 비교
tools/parity.sh                                     # 파서가 하나와 같은 트리를 만드는지 확인
```

`parity.sh`는 Go와, `haru` 옆 폴더에 있는 `hana` 저장소가 필요해요. 오류 문구와 표준 모듈 이름표는 하나의 것을 그대로 가져와 만들어요 ([`tools/errsgen`](tools/errsgen), [`tools/stdgen`](tools/stdgen)).

</details>

## 라이선스

[MIT License](LICENSE)예요.
