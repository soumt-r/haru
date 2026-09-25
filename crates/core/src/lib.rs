//! Haru runtime core. See `DESIGN.md` at the repository root.
//!
//! For now: values in the ABI layout, the module registry, and native calls
//! (static and dynamic). The compiler and VM come next (M1–M2).

mod dylib;
mod error;
mod host;
mod modules;
pub mod value;

pub use dylib::library_file_name;
pub use error::RuntimeError;
pub use modules::{FnRef, FunctionInfo, LoadError, ModuleInfo, Runtime};
pub use value::Value;
