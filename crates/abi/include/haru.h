// haru.h: the Haru native module ABI, version 1, for C (and C++, Zig, ...).
//
// GENERATED from crates/abi/src/lib.rs by crates/abi/tests/header.rs; do not
// edit. `HARU_BLESS=1 cargo test -p haru-abi` writes it anew.
//
// A module is a shared library (.dll, .so, .dylib) that exports one function,
// `const HaruModuleDesc *haru_module_v1_<id>(const HaruHostApi *host)`, and a
// `haru.toml` naming it (`[native] id = "<id>"`). See examples/greet_c.

#ifndef HARU_H
#define HARU_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct HaruRawValue HaruRawValue;
typedef struct HaruStr HaruStr;
typedef struct HaruHostCtx HaruHostCtx;
typedef struct HaruName HaruName;
typedef struct HaruFunctionDesc HaruFunctionDesc;
typedef struct HaruMessageDesc HaruMessageDesc;
typedef struct HaruResourceDesc HaruResourceDesc;
typedef struct HaruModuleDesc HaruModuleDesc;
typedef struct HaruHostApi HaruHostApi;

#define HARU_ABI_VERSION 1

// Prefix of the symbol a dynamic library exports; the module id follows it
// (`haru_module_v1_greet`), so several modules can be linked into one binary.
#define HARU_ENTRY_PREFIX "haru_module_v1_"

// What a `RawValue` holds. Tags from `tag::STR` on point at host-owned,
// reference-counted objects.
#define HARU_TAG_NULL 0
#define HARU_TAG_BOOL 1
#define HARU_TAG_NUM 2
#define HARU_TAG_STR 3
#define HARU_TAG_LIST 4
#define HARU_TAG_DICT 5
#define HARU_TAG_FUNC 6
#define HARU_TAG_OBJECT 7
#define HARU_TAG_RESOURCE 8

// The kind a parameter accepts. The host checks every argument against it
// before the call, so a function never sees a value of the wrong kind.
#define HARU_KIND_ANY 0
#define HARU_KIND_NUM 1
#define HARU_KIND_STR 2
#define HARU_KIND_BOOL 3
#define HARU_KIND_LIST 4
#define HARU_KIND_DICT 5
#define HARU_KIND_FUNC 6
// A resource (any type: the function checks which with `resource_get`).
#define HARU_KIND_RESOURCE 7
// As the only parameter: any number of arguments of any kind, which the
// function checks itself (the standard library, to report Hana's errors).
#define HARU_KIND_REST 99

// A value, 16 bytes. `NULL`/`BOOL`/`NUM` keep their data in `payload`
// (a bool is 0 or 1, a number is the bits of an `f64`); heap tags keep a
// pointer the host owns and only the host may dereference.
struct HaruRawValue {
    uint32_t tag;
    uint32_t pad;
    uint64_t payload;
};

// A borrowed UTF-8 string: `ptr` is valid for `len` bytes for as long as the
// thing it was taken from.
struct HaruStr {
    const uint8_t *ptr;
    size_t len;
};

// Returned by native functions and by host functions that can fail.
typedef uint32_t HaruStatus;

#define HARU_STATUS_OK 0

// An error is pending on the call context (set with `HostApi::throw`).
#define HARU_STATUS_ERROR 1

// The host's state for one native call. Opaque to modules.
// A native function.
//
// `args` are borrowed for the duration of the call; their count and kinds
// already match the descriptor. On success the function writes an owned value
// (+1 reference) to `out` and returns `STATUS_OK`; on failure it calls one of
// the host's `throw` functions and returns what it returned.
typedef HaruStatus (*HaruNativeFn)(const void *userdata, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out);

// A name in one language (`"hari"`, `"kanade"`).
struct HaruName {
    HaruStr lang;
    HaruStr name;
};

struct HaruFunctionDesc {
    // Language-neutral id, unique in the module (`"ceil"`).
    HaruStr id;
    const HaruName *names;
    size_t names_len;
    // One `kind` per parameter.
    const uint32_t *params;
    size_t params_len;
    // How many leading parameters must be given; the rest may be left out.
    size_t required;
    HaruNativeFn func;
    const void *userdata;
};

// A message template for an error code in one language. `{0}`, `{1}`, ... are
// replaced with the error's arguments.
struct HaruMessageDesc {
    HaruStr code;
    HaruStr lang;
    HaruStr template_;
};

// A kind of native object (a resource): what the program sees as a value
// with methods, and what frees it when the last reference goes.
struct HaruResourceDesc {
    // Language-neutral id, unique in the module (`"connection"`).
    HaruStr id;
    const HaruName *names;
    size_t names_len;
    // Frees the object `resource_new` was given.
    void (*drop)(void *ptr);
    // Its methods; each receives the resource as its first argument.
    const HaruFunctionDesc *methods;
    size_t methods_len;
};

struct HaruModuleDesc {
    uint32_t abi_version;
    // Language-neutral id (`"math"`).
    HaruStr id;
    const HaruName *names;
    size_t names_len;
    const HaruFunctionDesc *functions;
    size_t functions_len;
    const HaruMessageDesc *messages;
    size_t messages_len;
    const HaruResourceDesc *resources;
    size_t resources_len;
};

// The module entry point.
typedef const HaruModuleDesc *(*HaruEntryFn)(const HaruHostApi *host);

// Functions the host gives to modules. Later versions only append fields;
// `size` tells a module which ones exist.
struct HaruHostApi {
    uint32_t abi_version;
    uint32_t size;

    // Add / drop one reference. No-ops for non-heap tags.
    void (*retain)(HaruRawValue v);
    void (*release)(HaruRawValue v);

    // The text of a string value, valid while the value is alive.
    HaruStr (*str_view)(HaruRawValue v);
    // A new string (+1) holding a copy of `s`.
    HaruRawValue (*str_new)(HaruStr s);

    // A new, empty list (+1).
    HaruRawValue (*list_new)(size_t capacity);
    size_t (*list_len)(HaruRawValue list);
    // The item at a 0-based `index` (+1), or null when out of range.
    HaruRawValue (*list_get)(HaruRawValue list, size_t index);
    // Appends `item`, taking over its reference.
    void (*list_push)(HaruRawValue list, HaruRawValue item);

    // Calls a function value. On success `out` receives the result (+1).
    // On failure the error is already pending on `ctx`: return the status.
    HaruStatus (*call)(HaruHostCtx *ctx, HaruRawValue func, const HaruRawValue *args, size_t argc, HaruRawValue *out);

    // Raises the calling module's error `code` with arguments (borrowed; the
    // host keeps its own references) and returns `STATUS_ERROR`.
    HaruStatus (*throw_)(HaruHostCtx *ctx, HaruStr code, const HaruRawValue *args, size_t argc);
    // Raises the host's argument-type error for the 0-based parameter `index`.
    HaruStatus (*throw_type)(HaruHostCtx *ctx, size_t index, uint32_t expected);

    // Replaces the item at a 0-based `index`, taking over `item`'s
    // reference; false (and `item` dropped) when out of range.
    bool (*list_set)(HaruRawValue list, size_t index, HaruRawValue item);

    // A new, empty dictionary (+1).
    HaruRawValue (*dict_new)(void);
    size_t (*dict_len)(HaruRawValue dict);
    // The value under `key` (borrowed) into `out` (+1); false when absent.
    bool (*dict_get)(HaruRawValue dict, HaruRawValue key, HaruRawValue *out);
    // Sets `key` to `value`, taking over both references.
    void (*dict_set)(HaruRawValue dict, HaruRawValue key, HaruRawValue value);
    // The keys as a new list (+1), in no particular order.
    HaruRawValue (*dict_keys)(HaruRawValue dict);

    // A new resource (+1) of a kind this module describes, owning `ptr`
    // (freed by the kind's `drop` when the last reference goes).
    HaruRawValue (*resource_new)(const HaruResourceDesc *kind, void *ptr);
    // The object of a resource of that kind (borrowed), or null.
    void *(*resource_get)(HaruRawValue v, const HaruResourceDesc *kind);
    // Takes the error a failed `call` left pending: its message in a locale
    // (0 English, 1 Korean, 2 Japanese) as a new string (+1). False when
    // there is none. The error is then gone, as if it was caught.
    bool (*take_error)(HaruHostCtx *ctx, uint32_t locale, HaruRawValue *out);
    // Writes out what the program has printed so far: call it before
    // waiting (a sleep, a network wait), so the output shows meanwhile.
    void (*flush)(HaruHostCtx *ctx);
    // How many parameters a function value declares, or -1 when it takes
    // any number (or is not a function). A function may be given fewer
    // arguments than that, never more. (Check `size` before using it.)
    int64_t (*func_params)(HaruHostCtx *ctx, HaruRawValue func);
    // Removes `key` (borrowed) from a dictionary; false when it was not
    // there. (Check `size` before using it.)
    bool (*dict_remove)(HaruRawValue dict, HaruRawValue key);
};

// ---- conveniences (not part of the Rust definitions)

#if defined(__cplusplus)
static_assert(sizeof(HaruRawValue) == 16, "a value is 16 bytes");
#else
_Static_assert(sizeof(HaruRawValue) == 16, "a value is 16 bytes");
#endif

// Marks the entry function as exported from the shared library.
#if defined(_WIN32)
#define HARU_EXPORT __declspec(dllexport)
#else
#define HARU_EXPORT __attribute__((visibility("default")))
#endif

// A string literal as a HaruStr (`HARU_STR("ceil")`).
// The same as an initializer, for static tables (`HaruName n = { HARU_STR_INIT("hari"), ... }`).
#define HARU_STR_INIT(s) { (const uint8_t *)(s), sizeof(s) - 1 }
#ifdef __cplusplus
#define HARU_STR(s) (HaruStr{ (const uint8_t *)(s), sizeof(s) - 1 })
#else
#define HARU_STR(s) ((HaruStr){ (const uint8_t *)(s), sizeof(s) - 1 })
#endif

static inline HaruRawValue haru_null(void) {
    HaruRawValue v = { HARU_TAG_NULL, 0, 0 };
    return v;
}

static inline HaruRawValue haru_bool(bool b) {
    HaruRawValue v = { HARU_TAG_BOOL, 0, b ? 1u : 0u };
    return v;
}

static inline HaruRawValue haru_num(double n) {
    HaruRawValue v = { HARU_TAG_NUM, 0, 0 };
    memcpy(&v.payload, &n, sizeof n);
    return v;
}

// The number a HARU_TAG_NUM value holds.
static inline double haru_as_num(HaruRawValue v) {
    double n;
    memcpy(&n, &v.payload, sizeof n);
    return n;
}

#ifdef __cplusplus
}
#endif

#endif
