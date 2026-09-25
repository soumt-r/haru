//! The module registry and native calls.
//!
//! Every module — the standard library linked into the binary, or a package
//! loaded from a dynamic library — arrives as the same [`ModuleDesc`] and is
//! found by its name in the program's language.

use std::collections::HashMap;
use std::path::Path;

use haru_abi::{
    kind, EntryFn, FunctionDesc, HostCtx, ModuleDesc, Name, RawValue, ABI_VERSION, STATUS_OK,
};

use crate::error::RuntimeError;
use crate::host::{CallCtx, HOST_API};
use crate::value::{FuncObj, Value};

/// A module function, found with [`Runtime::function`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FnRef {
    pub module: usize,
    pub func: usize,
}

pub(crate) struct LoadedModule {
    pub id: String,
    names: HashMap<String, String>,
    functions: Vec<LoadedFn>,
    /// (language, name) -> index into `functions`
    by_name: HashMap<(String, String), usize>,
    /// (code, language) -> template
    messages: HashMap<(String, String), String>,
}

pub(crate) struct LoadedFn {
    pub id: String,
    names: HashMap<String, String>,
    desc: *const FunctionDesc,
    params: Vec<u32>,
    required: usize,
}

#[derive(Debug)]
pub enum LoadError {
    /// The entry returned nothing or a descriptor of another ABI version.
    Incompatible(String),
    /// Another module already has this id.
    Duplicate(String),
    Library(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Incompatible(m) => write!(f, "incompatible module: {m}"),
            LoadError::Duplicate(m) => write!(f, "module loaded twice: {m}"),
            LoadError::Library(m) => write!(f, "cannot load library: {m}"),
        }
    }
}

impl std::error::Error for LoadError {}

#[derive(Default)]
pub struct Runtime {
    modules: Vec<LoadedModule>,
    /// (language, module name) -> module index
    by_name: HashMap<(String, String), usize>,
}

impl Runtime {
    pub fn new() -> Runtime {
        Runtime::default()
    }

    /// Registers a module linked into this binary.
    pub fn load_static(&mut self, entry: EntryFn) -> Result<usize, LoadError> {
        let desc = unsafe { entry(&HOST_API) };
        unsafe { self.register(desc) }
    }

    /// Loads a module from a dynamic library exporting `haru_module_v1_<id>`.
    /// The library stays loaded for the rest of the process.
    pub fn load_dynamic(&mut self, path: &Path, id: &str) -> Result<usize, LoadError> {
        let symbol = format!("{}{}", haru_abi::ENTRY_PREFIX, id);
        let entry = crate::dylib::open(path, &symbol).map_err(LoadError::Library)?;
        let desc = unsafe { entry(&HOST_API) };
        unsafe { self.register(desc) }
    }

    unsafe fn register(&mut self, desc: *const ModuleDesc) -> Result<usize, LoadError> {
        if desc.is_null() {
            return Err(LoadError::Incompatible("entry returned no descriptor".into()));
        }
        let desc = &*desc;
        if desc.abi_version != ABI_VERSION {
            return Err(LoadError::Incompatible(format!("ABI version {}", desc.abi_version)));
        }
        let id = desc.id.as_str().to_string();
        if self.modules.iter().any(|m| m.id == id) {
            return Err(LoadError::Duplicate(id));
        }

        let mut functions = Vec::new();
        let mut by_name = HashMap::new();
        for (index, f) in slice(desc.functions, desc.functions_len).iter().enumerate() {
            let names = names_of(f.names, f.names_len);
            for (lang, name) in &names {
                by_name.insert((lang.clone(), name.clone()), index);
            }
            let params = slice(f.params, f.params_len).to_vec();
            functions.push(LoadedFn {
                id: f.id.as_str().to_string(),
                names,
                desc: f,
                required: f.required.min(params.len()),
                params,
            });
        }
        let messages = slice(desc.messages, desc.messages_len)
            .iter()
            .map(|m| ((m.code.as_str().to_string(), m.lang.as_str().to_string()), m.template.as_str().to_string()))
            .collect();

        let index = self.modules.len();
        let names = names_of(desc.names, desc.names_len);
        for (lang, name) in &names {
            self.by_name.insert((lang.clone(), name.clone()), index);
        }
        self.modules.push(LoadedModule { id, names, functions, by_name, messages });
        Ok(index)
    }

    /// The module a program in `lang` calls `name` (`[수학]` in Hari).
    pub fn module(&self, lang: &str, name: &str) -> Option<usize> {
        self.by_name.get(&(lang.to_string(), name.to_string())).copied()
    }

    /// A function of a module by its name in `lang` (`<올림>`).
    pub fn function(&self, module: usize, lang: &str, name: &str) -> Option<FnRef> {
        let m = self.modules.get(module)?;
        let func = *m.by_name.get(&(lang.to_string(), name.to_string()))?;
        Some(FnRef { module, func })
    }

    /// A function value that the program can store and pass on.
    pub fn function_value(&self, f: FnRef) -> Value {
        Value::func(FuncObj { module: f.module, func: f.func })
    }

    /// Calls a native function. The count and kinds of the arguments are
    /// checked here, so modules never see mismatched values.
    pub fn call(&self, f: FnRef, args: &[Value]) -> Result<Value, RuntimeError> {
        let func = &self.modules[f.module].functions[f.func];
        if args.len() < func.required || args.len() > func.params.len() {
            return Err(RuntimeError::core("ArgumentCount")
                .arg(Value::num(func.params.len() as f64))
                .arg(Value::num(args.len() as f64)));
        }
        for (i, (arg, &want)) in args.iter().zip(&func.params).enumerate() {
            if !kind::accepts(want, arg.tag()) {
                return Err(type_error(i, want));
            }
        }

        let mut ctx = CallCtx { rt: self, module: f.module, pending: None };
        let mut out = RawValue::NULL;
        let desc = unsafe { &*func.desc };
        // `Value` is `repr(transparent)` over `RawValue`: the arguments go as they are.
        let status = unsafe {
            (desc.func)(
                desc.userdata,
                &mut ctx as *mut CallCtx as *mut HostCtx,
                args.as_ptr() as *const RawValue,
                args.len(),
                &mut out,
            )
        };
        if status == STATUS_OK {
            Ok(unsafe { Value::from_raw(out) })
        } else {
            Err(ctx.pending.take().unwrap_or_else(|| RuntimeError::core("NativeFailed")))
        }
    }

    /// Calls any function value (only native functions exist so far).
    pub fn call_value(&self, func: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
        match func.as_func() {
            Some(f) => self.call(FnRef { module: f.module, func: f.func }, args),
            None => Err(RuntimeError::core("NotCallable")),
        }
    }

    /// Language-neutral ids and names of every loaded module, for listings.
    pub fn describe(&self) -> Vec<ModuleInfo> {
        self.modules
            .iter()
            .map(|m| ModuleInfo {
                id: m.id.clone(),
                names: sorted(&m.names),
                functions: m
                    .functions
                    .iter()
                    .map(|f| FunctionInfo { id: f.id.clone(), names: sorted(&f.names), params: f.params.len() })
                    .collect(),
            })
            .collect()
    }

    pub(crate) fn module_id(&self, module: usize) -> &str {
        &self.modules[module].id
    }

    pub(crate) fn message(&self, module: usize, code: &str, lang: &str) -> Option<&str> {
        self.modules[module].messages.get(&(code.to_string(), lang.to_string())).map(|s| s.as_str())
    }
}

pub struct ModuleInfo {
    pub id: String,
    pub names: Vec<(String, String)>,
    pub functions: Vec<FunctionInfo>,
}

pub struct FunctionInfo {
    pub id: String,
    pub names: Vec<(String, String)>,
    pub params: usize,
}

pub(crate) fn type_error(index: usize, want: u32) -> RuntimeError {
    RuntimeError::core("ArgumentType").arg(Value::num(index as f64 + 1.0)).arg(Value::num(want as f64))
}

unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

unsafe fn names_of(ptr: *const Name, len: usize) -> HashMap<String, String> {
    slice(ptr, len).iter().map(|n| (n.lang.as_str().to_string(), n.name.as_str().to_string())).collect()
}

fn sorted(map: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<_> = map.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
    v.sort();
    v
}
