//! Compiles greet.c into a shared library with the platform's C compiler (as
//! the `cc` crate finds it), for Haru's tests. The path goes to the crate as
//! GREET_C_LIBRARY. Nothing here is needed to write a C package: see README.md.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let include = dir.join("../../crates/abi/include");
    let source = dir.join("greet.c");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", include.join("haru.h").display());

    let target = std::env::var("TARGET").unwrap();
    let file = if target.contains("windows") {
        "greet_c.dll"
    } else if target.contains("apple") {
        "libgreet_c.dylib"
    } else {
        "libgreet_c.so"
    };
    let library = out.join(file);

    let tool = cc::Build::new().get_compiler();
    let mut cmd: Command = tool.to_command();
    if tool.is_like_msvc() {
        cmd.args(["/nologo", "/utf-8", "/std:c11", "/LD", "/O2"])
            .arg(format!("/I{}", include.display()))
            .arg(&source)
            .arg(format!("/Fe{}", library.display()))
            .arg(format!("/Fo{}\\", out.display()));
    } else {
        cmd.args(["-std=c11", "-O2", "-shared", "-fPIC", "-fvisibility=hidden", "-Wall", "-Werror"])
            .arg(format!("-I{}", include.display()))
            .arg(&source)
            .arg("-o")
            .arg(&library);
    }
    let status = cmd.status().expect("the C compiler runs");
    assert!(status.success(), "greet.c did not compile");
    println!("cargo:rustc-env=GREET_C_LIBRARY={}", library.display());
}
