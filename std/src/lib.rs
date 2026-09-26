//! The standard library. Each module is written with `haru-sdk` exactly like a
//! third-party package; the only difference is that `haru` links it in. The
//! modules, functions and their names are Hana's (`names.rs`, generated from
//! hana/std by tools/stdgen), and so is their behaviour.

use haru_sdk::abi::EntryFn;
use haru_sdk::prelude::*;

mod csv;
mod datetime;
mod encoding;
mod file;
mod gosort;
mod gourl;
mod goregex;
mod hana;
mod http;
mod json;
mod list;
mod math;
mod net;
mod path;
mod random;
mod regexp;
mod regex_names;
mod stats;
mod text;
pub mod names;

pub use file::deny_files;
pub use net::deny_net;
pub use http::fetch;

use names::HANA_STD;

/// Every standard module Haru has, in load order.
pub const MODULES: &[EntryFn] = &[
    math::entry,
    text::entry,
    list::entry,
    json::entry,
    csv::entry,
    stats::entry,
    path::entry,
    encoding::entry,
    encoding::hash_entry,
    random::entry,
    regexp::entry,
    datetime::entry,
    file::entry,
    net::entry,
    http::entry,
];

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
