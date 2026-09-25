//! Names as small integers, unique in the process: variables, properties,
//! classes and methods are compared by number, not by text.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

struct Table {
    ids: HashMap<&'static str, u32>,
    names: Vec<&'static str>,
}

fn table() -> &'static Mutex<Table> {
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(Table { ids: HashMap::new(), names: Vec::new() }))
}

/// The number of a name (the same name always gets the same number).
pub fn intern(name: &str) -> u32 {
    let mut t = table().lock().unwrap();
    if let Some(&id) = t.ids.get(name) {
        return id;
    }
    let s: &'static str = Box::leak(name.to_string().into_boxed_str());
    let id = t.names.len() as u32;
    t.names.push(s);
    t.ids.insert(s, id);
    id
}

/// The name a number stands for.
pub fn name(id: u32) -> &'static str {
    table().lock().unwrap().names[id as usize]
}
