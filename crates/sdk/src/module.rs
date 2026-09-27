use std::ffi::c_void;

use haru_abi as abi;

use crate::{native_fn, Handler};

/// A module being described. See the crate docs.
pub struct Module {
    id: String,
    names: Vec<(String, String)>,
    funcs: Vec<FuncEntry>,
    messages: Vec<(String, String, String)>,
    resources: Vec<ResourceEntry>,
}

/// A kind of resource of a [`Module`]; returned by [`Module::resource`].
pub struct ResourceEntry {
    id: String,
    names: Vec<(String, String)>,
    type_id: std::any::TypeId,
    drop: unsafe extern "C" fn(*mut c_void),
    methods: Vec<FuncEntry>,
}

unsafe extern "C" fn drop_box<T>(ptr: *mut c_void) {
    drop(Box::from_raw(ptr as *mut T));
}

impl ResourceEntry {
    /// The kind's name in a language (what printing a resource shows).
    pub fn name(&mut self, lang: &str, name: &str) -> &mut ResourceEntry {
        self.names.push((lang.to_string(), name.to_string()));
        self
    }

    /// Adds a method: its first parameter is the resource (`Res<T>`), the
    /// rest are the arguments of `'값'의 <메서드>(...)`.
    pub fn method<A, H: Handler<A>>(&mut self, id: &str, handler: H) -> &mut FuncEntry {
        let kinds = H::kinds();
        self.methods.push(FuncEntry {
            id: id.to_string(),
            names: Vec::new(),
            required: H::required(),
            kinds,
            func: native_fn::<H, A>(),
            userdata: Box::into_raw(Box::new(handler)) as *const c_void,
        });
        self.methods.last_mut().unwrap()
    }
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
        Module { id: id.to_string(), names: Vec::new(), funcs: Vec::new(), messages: Vec::new(), resources: Vec::new() }
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
            required: H::required(),
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

    /// Adds a kind of resource: a Rust value of type `T` the program holds as
    /// a value with methods; it is dropped when the program lets go of it.
    pub fn resource<T: 'static>(&mut self, id: &str) -> &mut ResourceEntry {
        self.resources.push(ResourceEntry {
            id: id.to_string(),
            names: Vec::new(),
            type_id: std::any::TypeId::of::<T>(),
            drop: drop_box::<T>,
            methods: Vec::new(),
        });
        self.resources.last_mut().unwrap()
    }

    /// The message for one of this module's error codes in a language.
    pub fn message(&mut self, code: &str, lang: &str, template: &str) -> &mut Module {
        self.messages.push((code.to_string(), lang.to_string(), template.to_string()));
        self
    }

    /// Turns the description into a descriptor that lives for the rest of the
    /// process (a module is never unloaded).
    pub(crate) fn leak(mut self) -> *const abi::ModuleDesc {
        let functions = leak_functions(self.funcs);
        let resources: Vec<abi::ResourceDesc> = self
            .resources
            .iter_mut()
            .map(|r| {
                let names = leak_names(std::mem::take(&mut r.names));
                let methods = leak_functions(std::mem::take(&mut r.methods));
                abi::ResourceDesc {
                    id: leak_str(r.id.clone()),
                    names: names.as_ptr(),
                    names_len: names.len(),
                    drop: r.drop,
                    methods: methods.as_ptr(),
                    methods_len: methods.len(),
                }
            })
            .collect();
        let resources: &'static [abi::ResourceDesc] = Box::leak(resources.into_boxed_slice());
        for (r, desc) in self.resources.iter().zip(resources) {
            crate::register_kind(r.type_id, desc);
        }
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
            resources: resources.as_ptr(),
            resources_len: resources.len(),
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

fn leak_functions(funcs: Vec<FuncEntry>) -> &'static [abi::FunctionDesc] {
    let functions: Vec<abi::FunctionDesc> = funcs
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
    Box::leak(functions.into_boxed_slice())
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
