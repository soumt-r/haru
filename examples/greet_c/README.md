# greet_c: a package in C

The [greet](../greet) example package again, written in C against
[`haru.h`](../../crates/abi/include/haru.h) alone: greetings, a sum that
reports its own error, a list changed in place, a callback, and a resource
(a counter with methods) freed when the program lets go of it.

A C package is a shared library that exports one function,
`haru_module_v1_<id>`, and a `haru.toml` that names it:

```toml
[package]
name = "greet_c"
version = "0.1.0"

[native]
id = "greet_c"

[native.files.linux-amd64]
file = "native/libgreet_c.so"
```

## Building

```bash
# Linux
cc -std=c11 -O2 -shared -fPIC -fvisibility=hidden -I../../crates/abi/include greet.c -o native/libgreet_c.so
# macOS
cc -std=c11 -O2 -shared -fPIC -fvisibility=hidden -I../../crates/abi/include greet.c -o native/libgreet_c.dylib
# Windows (Developer Command Prompt)
cl /utf-8 /std:c11 /LD /O2 /I..\..\crates\abi\include greet.c /Fenative\greet_c.dll
```

Put the folder under your project's `packages/` (or name it in `$HARU_PACKAGES`),
then:

```hari
[greet_c]에서 <인사말>을 가져오자
<인사말>("하리")를 출력하자
```

`Cargo.toml`, `build.rs` and `lib.rs` only build it for Haru's own tests
(`crates/cli/tests/c_module.rs`); a C package needs none of them.
