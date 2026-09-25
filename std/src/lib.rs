//! The standard library. Each module is written with `haru-sdk` exactly like a
//! third-party package; the only difference is that `haru` links it in. The
//! modules, functions and their names are Hana's (`names.rs`, generated from
//! hana/std by tools/stdgen), and so is their behaviour.

use haru_sdk::abi::EntryFn;
use haru_sdk::prelude::*;

mod hana;
mod math;
pub mod names;

use names::HANA_STD;

/// Every standard module Haru has, in load order.
pub const MODULES: &[EntryFn] = &[math::entry];

/// Ids of the modules in [`MODULES`] (complete: every function Hana has).
const IMPLEMENTED: &[&str] = &["math"];

/// Hana packages that come with Hana (not standard modules, not here yet).
const HANA_PACKAGES: &[&str] = &["timezone", "http_server"];

/// What a `[모듈]` name means in a language: see `haru_core::compiler::StdLookup`.
pub fn lookup(lang: &str, name: &str) -> Option<(bool, Vec<String>)> {
    if HANA_PACKAGES.contains(&name) {
        return Some((false, Vec::new()));
    }
    let kanade = lang == "kanade";
    let m = HANA_STD.iter().find(|m| (if kanade { m.names.1 } else { m.names.0 }) == name)?;
    let fns = m.functions.iter().map(|f| if kanade { f.2 } else { f.1 }.to_string()).collect();
    Some((IMPLEMENTED.contains(&m.id), fns))
}

/// A standard function: it takes the arguments as they come and checks them itself.
pub(crate) type StdFn = fn(&[Value]) -> Result<Value>;

/// Names a module and its functions from the catalog: `fns` are (function
/// id, implementation) in the catalog's order.
pub(crate) fn describe(m: &mut Module, id: &str, fns: &[(&str, StdFn)]) {
    let module = HANA_STD.iter().find(|x| x.id == id).expect("a module of the catalog");
    m.name("hari", module.names.0).name("kanade", module.names.1);
    for (fid, f) in fns {
        let (_, hari, kanade) = module.functions.iter().find(|x| x.0 == *fid).expect("a function of the catalog");
        m.raw(fid, *f).name("hari", hari).name("kanade", kanade);
    }
    debug_assert_eq!(fns.len(), module.functions.len(), "{id} must implement every function");
}
