//! The standard library. Each module is written with `haru-sdk` exactly like a
//! third-party package; the only difference is that `haru` links it in.

use haru_sdk::abi::EntryFn;

mod math;

/// Every standard module, in load order.
pub const MODULES: &[EntryFn] = &[math::entry];
