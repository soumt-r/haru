//! Where build.rs put the C example as a shared library (for Haru's tests).

/// The path of the compiled greet.c (`greet_c.dll`, `libgreet_c.so`, ...).
pub const LIBRARY: &str = env!("GREET_C_LIBRARY");
