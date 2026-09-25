use std::ffi::c_void;

use haru_abi as abi;

use crate::{native_fn, Handler};

/// A module being described. See the crate docs.
pub struct Module {
    id: String,
    names: Vec<(String, String)>,
    funcs: Vec<FuncEntry>,
    messages: Vec<(String, String, String)>,
}

/// One function of a [`Module`]; returned by [`Module::func`] to add names.
pub struct FuncEntry {
    id: String,
    names: Vec<(String, String)>,
    kinds: Vec<u32>,
    required: usize,
    func: abi::NativeFn,
    userdata: *const c_void,
}

impl Module {
    pub(crate) fn new(id: &str) -> Module {
        Module { id: id.to_string(), names: Vec::new(), funcs: Vec::new(), messages: Vec::new() }
    }

    /// The module's name in a language (`m.name("hari", "수학")`).
    pub fn name(&mut self, lang: &str, name: &str) -> &mut Module {
        self.names.push((lang.to_string(), name.to_string()));
        self
    }

    /// Adds a function. Its parameter kinds come from the closure's types.
    pub fn func<A, H: Handler<A>>(&mut self, id: &str, handler: H) -> &mut FuncEntry {
        let kinds = H::kinds();
        self.funcs.push(FuncEntry {
            id: id.to_string(),
            names: Vec::new(),
            required: kinds.len(),
            kinds,
            func: native_fn::<H, A>(),
            // Lives as long as the module, which is never unloaded.
            userdata: Box::into_raw(Box::new(handler)) as *const c_void,
        });
        self.funcs.last_mut().unwrap()
    }

    /// Adds a function that takes its arguments as they come, any number and
    /// kind, and checks them itself (to report errors exactly as it wants).
    pub fn raw(&mut self, id: &str, f: fn(&[crate::Value]) -> crate::Result<crate::Value>) -> &mut FuncEntry {
        self.funcs.push(FuncEntry {
            id: id.to_string(),
            names: Vec::new(),
            required: 0,
            kinds: vec![abi::kind::REST],
            func: crate::raw_shim,
            userdata: f as *const c_void,
        });
        self.funcs.last_mut().unwrap()
    }

    /// The message for one of this module's error codes in a language.
    pub fn message(&mut self, code: &str, lang: &str, template: &str) -> &mut Module {
        self.messages.push((code.to_string(), lang.to_string(), template.to_string()));
        self
    }

    /// Turns the description into a descriptor that lives for the rest of the
    /// process (a module is never unloaded).
    pub(crate) fn leak(self) -> *const abi::ModuleDesc {
        let functions: Vec<abi::FunctionDesc> = self
            .funcs
            .into_iter()
            .map(|f| {
                let names = leak_names(f.names);
                let params = Box::leak(f.kinds.into_boxed_slice());
                abi::FunctionDesc {
                    id: leak_str(f.id),
                    names: names.as_ptr(),
                    names_len: names.len(),
                    params: params.as_ptr(),
                    params_len: params.len(),
                    required: f.required,
                    func: f.func,
                    userdata: f.userdata,
                }
            })
            .collect();
        let functions = Box::leak(functions.into_boxed_slice());
        let messages: Vec<abi::MessageDesc> = self
            .messages
            .into_iter()
            .map(|(code, lang, template)| abi::MessageDesc {
                code: leak_str(code),
                lang: leak_str(lang),
                template: leak_str(template),
            })
            .collect();
        let messages = Box::leak(messages.into_boxed_slice());
        let names = leak_names(self.names);
        Box::leak(Box::new(abi::ModuleDesc {
            abi_version: abi::ABI_VERSION,
            id: leak_str(self.id),
            names: names.as_ptr(),
            names_len: names.len(),
            functions: functions.as_ptr(),
            functions_len: functions.len(),
            messages: messages.as_ptr(),
            messages_len: messages.len(),
        }))
    }
}

impl FuncEntry {
    /// The function's name in a language (`.name("hari", "올림")`).
    pub fn name(&mut self, lang: &str, name: &str) -> &mut FuncEntry {
        self.names.push((lang.to_string(), name.to_string()));
        self
    }
}

fn leak_str(s: String) -> abi::Str {
    abi::Str::new(Box::leak(s.into_boxed_str()))
}

fn leak_names(names: Vec<(String, String)>) -> &'static [abi::Name] {
    let names: Vec<abi::Name> = names
        .into_iter()
        .map(|(lang, name)| abi::Name { lang: leak_str(lang), name: leak_str(name) })
        .collect();
    Box::leak(names.into_boxed_slice())
}
