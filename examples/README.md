# Example packages

The same small package (greetings, a sum with its own error, a list changed
in place, a callback, a counter) written three ways:

| Folder | Written in | How it reaches the program |
| --- | --- | --- |
| [`greet`](greet) | Rust, with [`haru-sdk`](../crates/sdk) | a native module: a shared library, or linked into a `haru build` binary |
| [`greet_c`](greet_c) | C, with [`haru.h`](../crates/abi/include/haru.h) | a native module: a shared library |
| [`greet_source`](greet_source) | Hari and Kanade | source entry points (the same folder also works in Hana) |

A package may mix them, too: `packages/timezone` has Hari and Kanade entry
points over a native module written in Rust.
