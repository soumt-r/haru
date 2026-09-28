# greet_source: a package in Hari and Kanade

The [greet](../greet) example package again, in source only: an entry point
per language (`hari/index.hr`, `kanade/index.knd`) and no native code. Where
the Rust and C versions make a resource, this one declares a class.

Because it is source only, the same folder is a package for Haru
(`haru.toml`) and for Hana (`hana.pkg.json`). Put it under your project's
`packages/`, then:

```hari
[greet_source]에서 전부 가져오자
<인사말>("하리")를 출력하자
'c'를 새로운 [계수기](10)로 정하자
('c'의 <더하기>(5))를 출력하자
```

```kanade
【greet_source】から全部持ってこよう
〈挨拶文〉(「ハリ」)を出力しよう
```

Haru's tests use it from a project: `crates/cli/tests/source_package.rs`.
