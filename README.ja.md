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
  <b>Haru</b> runs <b>Hari</b> (ハリ) and <b>Kanade</b> (カナデ),<br>
  the programming languages you write in Korean and Japanese, in Rust.<br>
  <sub>名前の<b>ハル</b>は、日本語の「春(はる)」と、韓国語の「하루(day)」の両方の意味を持ちます。</sub>
</p>

<br>

<table>
<tr>
<th align="center">ハリ (Hari) · 韓国語</th>
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

## ハナとハル

ハリとカナデの基準となるランタイムは、Goで作った[**ハナ**](https://github.com/soumt-r/hana)です。ハルは、同じ言語をRustで一から作り直したランタイムです。

- **言語はまったく同じです。** 同じプログラムはハナと同じ出力になり、エラーメッセージまで同じです。何千ものプログラムを両方のランタイムで動かして、結果を突き合わせながら作っています。
- **実行の仕組みは新しく設計しました。** 16バイトの値、レジスタVM、そして必要なときにオンにできる[Cranelift](https://cranelift.dev)のJITで、より速く動きます。
- **パッケージをRustで書けます。** Rustの関数をそのままハリ・カナデの関数として公開でき、プログラムと一緒に1つの実行ファイルにまとめられます。

設計とその理由は[DESIGN.md](DESIGN.md)に詳しく書いてあります(韓国語)。

## クイックスタート

[Rust](https://rustup.rs) (stable) が必要です。

```bash
cargo build --release                         # target/release/haru ができます
./target/release/haru run hello.knd           # カナデのプログラムを実行 (.hr はハリ)
./target/release/haru run --jit hello.knd     # JITをオンにして実行
```

`--time` を付けると、解析・コンパイルと実行にかかった時間を表示します。

## コマンド

| コマンド | すること |
| --- | --- |
| `haru run <ファイル>` | `.hr`(ハリ)や `.knd`(カナデ)のプログラムを実行します |
| `haru run --jit <ファイル>` | よく動く関数を機械語にしながら実行します |
| `haru build [--with <パッケージ>]... [-o <ファイル>] [<プログラム>]` | パッケージ(とプログラム)を1つの実行ファイルにまとめます |
| `haru init` | このフォルダに `haru.toml` を作ります |
| `haru add <gitパス>[@バージョン]` | パッケージを追加し、`haru.toml` と `haru.lock` に書きます |
| `haru install` · `haru remove <gitパス>` · `haru list` | パッケージのダウンロード、削除、一覧 |
| `haru modules [--lang kanade]` | 使える標準モジュールと関数の名前を表示します |
| `haru call <モジュール> <関数> [引数...]` | モジュールの関数を直接呼んでみます (`haru call --lang kanade 数学 階乗 5`) |
| `haru dis <ファイル>` | コンパイルしたレジスタコードを表示します |
| `haru version` | バージョンを表示します |

`--allow-file=false` と `--allow-net=false` は、ハナと同じく `【ファイル】`、`【ソケット】`、`【HTTP】` モジュールを使えなくします。

## どれくらい速い?

同じコンピュータで、プログラムの中で測った実行時間です(何度か測ったうちの最速、ミリ秒)。

| プログラム | ハナ | ハル | ハル `--jit` |
| --- | ---: | ---: | ---: |
| フィボナッチの再帰 (`fib(30)`) | 154 | 127 | **35** |
| 数値のループ | 96 | 48 | **11** |
| 算術 | 223 | 68 | **10** |
| クラスとメソッド | 56 | 26 | **20** |
| アヒインタプリタで「99本のビール」 | 172 | 140 | **107** |
| ブレインファックインタプリタ (約200万ステップ) | 511 | 376 | **118** |

プログラムの起動から終了までの全体の時間も短く、小さな例を1つ動かすのに、ハナは0.4秒ほど、ハルは0.07秒ほどです。

<details>
<summary><b>JITのしくみ</b></summary>

<br>

- 最初はすべてのコードをインタプリタが実行します。1000回ほど呼ばれたり回ったりした関数だけを別のスレッドで機械語にコンパイルし、準備ができたらそこから切り替えます。
- 数値だけを扱うループは値をレジスタに置いたまま回り、コンパイルされた関数どうしはVMのフレームなしで直接呼び合います。
- コンパイルされたコードが知らない場合に出会うと、その命令1つだけをインタプリタに任せて、また続けて走ります。だからJITをオンにしても結果はいつも同じです。
- すぐに終わるプログラムは、コンパイルが終わる前に終わってしまうので、JITの恩恵を受けられません。

環境変数でもオン・オフできます。`HARU_JIT=1` でオン、`HARU_JIT=0` でオフ、`HARU_JIT_HOT=<回数>` はコンパイルを始める基準です(0なら最初からコンパイルします)。

</details>

## パッケージ

パッケージは `haru.toml` があるフォルダです。ハリ・カナデのソースで書くことも、[`crates/sdk`](crates/sdk) で書いたRustのネイティブモジュールを一緒に置くこともできます。

```rust
use haru_sdk::prelude::*;

fn build(m: &mut Module) {
    m.name("hari", "인사").name("kanade", "挨拶");
    m.func("hello", |who: Str| format!("안녕, {who}!"))
        .name("hari", "인사말")
        .name("kanade", "挨拶文");
}
```

同じコードが動的ライブラリ(`.dll`/`.so`/`.dylib`)としても、`haru build --with` で実行ファイルに静的リンクしても動きます。例は [`examples/greet`](examples/greet) にあります。

| パッケージ | すること |
| --- | --- |
| [`http_server`](packages/http_server) | HTTPサーバー |
| [`timezone`](packages/timezone) | IANAタイムゾーンでの時刻の書式・読み取り・曜日・オフセット |

外部パッケージはハナと同じく git のパスが名前です。`haru add github.com/持ち主/リポジトリ` で追加し、`【github.com/持ち主/リポジトリ】から〈関数〉を持ってこよう` で使います。

## リポジトリの案内

<details>
<summary><b>フォルダ構成</b></summary>

<br>

| フォルダ | 内容 |
| --- | --- |
| [`crates/syntax`](crates/syntax) | ハリ・カナデのレキサーとパーサー(ハナと同じ木を作ります) |
| [`crates/core`](crates/core) | 値、コンパイラ、レジスタVM、JIT([`src/vm/jit.rs`](crates/core/src/vm/jit.rs))、モジュールレジストリ |
| [`crates/abi`](crates/abi), [`crates/sdk`](crates/sdk) | ネイティブモジュールのABIと、Rustでモジュールを書くSDK |
| [`crates/cli`](crates/cli) | `haru` コマンド |
| [`std/`](std) | SDKで書いた標準モジュール(実行ファイルに静的リンクされます) |
| [`packages/`](packages), [`examples/`](examples) | 同梱のパッケージと、例のパッケージ |
| [`tools/`](tools) | ハナと比べるツール(`runcheck.sh`、`jitcheck.sh`、`parity.sh`、エラーメッセージ・標準名前表の生成) |

</details>

<details>
<summary><b>テストとハナとの比較</b></summary>

<br>

```bash
cargo test
tools/runcheck.sh <hana> <haru> <プログラムのフォルダ>    # ハナとハルの出力を比較
tools/jitcheck.sh <haru> <プログラムのフォルダ>           # JITのオンとオフで比較
tools/parity.sh                                          # パーサーがハナと同じ木を作るか確認
```

`parity.sh` には Go と、`haru` の隣のフォルダにある `hana` リポジトリが必要です。エラーメッセージと標準モジュールの名前表は、ハナのものをそのまま取り込んで作ります([`tools/errsgen`](tools/errsgen)、[`tools/stdgen`](tools/stdgen))。

</details>

## ライセンス

[MIT License](LICENSE)です。
