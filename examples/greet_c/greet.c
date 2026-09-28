/* The greet package in C: the same module as examples/greet (Rust), written
 * against haru.h only. Build it as a shared library exporting
 * haru_module_v1_greet_c (see README.md). */

#include <stdlib.h>
#include <string.h>

#include "haru.h"

/* The host's functions, given to the entry point. */
static const HaruHostApi *host;

/* How many counters have been freed (to watch resources going). */
static size_t dropped_count;

/* `<인사말>("하리")`: "안녕, 하리!" */
static HaruStatus hello(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)argc;
    static const char before[] = "안녕, ", after[] = "!";
    HaruStr who = host->str_view(args[0]);
    size_t n = sizeof before - 1 + who.len + sizeof after - 1;
    char *text = malloc(n ? n : 1);
    memcpy(text, before, sizeof before - 1);
    if (who.len) memcpy(text + sizeof before - 1, who.ptr, who.len);
    memcpy(text + sizeof before - 1 + who.len, after, sizeof after - 1);
    HaruStr s = { (const uint8_t *)text, n };
    *out = host->str_new(s);
    free(text);
    return HARU_STATUS_OK;
}

/* `<합계>([1, 2, 3])`: the sum; an item that is no number is this module's
 * error NotNumber, with the item's position. */
static HaruStatus sum(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)argc;
    size_t n = host->list_len(args[0]);
    double total = 0;
    for (size_t i = 0; i < n; i++) {
        HaruRawValue item = host->list_get(args[0], i);
        if (item.tag != HARU_TAG_NUM) {
            host->release(item);
            HaruRawValue at = haru_num((double)(i + 1));
            return host->throw_(ctx, HARU_STR("NotNumber"), &at, 1);
        }
        total += haru_as_num(item);
    }
    *out = haru_num(total);
    return HARU_STATUS_OK;
}

/* `<두번추가>('목록', 값)`: the list is the program's own, so it sees both. */
static HaruStatus push_twice(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)argc;
    /* list_push takes over a reference: one of our own for each push. */
    host->retain(args[1]);
    host->list_push(args[0], args[1]);
    host->retain(args[1]);
    host->list_push(args[0], args[1]);
    *out = haru_null();
    return HARU_STATUS_OK;
}

/* `<두번적용>(<함수>, 값)`: f(f(x)); an error inside f passes through. */
static HaruStatus apply_twice(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)argc;
    HaruRawValue once;
    HaruStatus st = host->call(ctx, args[0], &args[1], 1, &once);
    if (st != HARU_STATUS_OK) return st;
    st = host->call(ctx, args[0], &once, 1, out);
    host->release(once);
    return st;
}

/* ---- a resource: a counter the program holds as a value with methods */

typedef struct Counter {
    double n;
} Counter;

static void counter_drop(void *p) {
    free(p);
    dropped_count++;
}

/* The counter's kind (resources[0] below), for resource_get and resource_new. */
static const HaruResourceDesc *counter_kind;

static HaruStatus counter_add(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)argc;
    Counter *c = host->resource_get(args[0], counter_kind);
    c->n += haru_as_num(args[1]);
    *out = haru_num(c->n);
    return HARU_STATUS_OK;
}

static HaruStatus counter_value(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)argc;
    Counter *c = host->resource_get(args[0], counter_kind);
    *out = haru_num(c->n);
    return HARU_STATUS_OK;
}

/* `<계수기>(10)`: a new counter. */
static HaruStatus counter_new(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)argc;
    Counter *c = malloc(sizeof *c);
    c->n = haru_as_num(args[0]);
    *out = host->resource_new(counter_kind, c);
    return HARU_STATUS_OK;
}

static HaruStatus dropped(const void *ud, HaruHostCtx *ctx, const HaruRawValue *args, size_t argc, HaruRawValue *out) {
    (void)ud, (void)ctx, (void)args, (void)argc;
    *out = haru_num((double)dropped_count);
    return HARU_STATUS_OK;
}

/* ---- what the module says it has */

#define NAMES(hari, kanade) { { HARU_STR_INIT("hari"), HARU_STR_INIT(hari) }, { HARU_STR_INIT("kanade"), HARU_STR_INIT(kanade) } }

static const HaruName hello_names[] = NAMES("인사말", "挨拶文");
static const HaruName sum_names[] = NAMES("합계", "合計");
static const HaruName push_twice_names[] = NAMES("두번추가", "二回追加");
static const HaruName apply_twice_names[] = NAMES("두번적용", "二回適用");
static const HaruName counter_names[] = NAMES("계수기", "カウンター");
static const HaruName dropped_names[] = NAMES("해제된수", "解放数");
static const HaruName add_names[] = NAMES("더하기", "足す");
static const HaruName value_names[] = NAMES("값", "値");
static const HaruName module_names[] = NAMES("C인사", "C挨拶");

static const uint32_t str_param[] = { HARU_KIND_STR };
static const uint32_t list_param[] = { HARU_KIND_LIST };
static const uint32_t list_any_params[] = { HARU_KIND_LIST, HARU_KIND_ANY };
static const uint32_t func_any_params[] = { HARU_KIND_FUNC, HARU_KIND_ANY };
static const uint32_t num_param[] = { HARU_KIND_NUM };
static const uint32_t resource_param[] = { HARU_KIND_RESOURCE };
static const uint32_t resource_num_params[] = { HARU_KIND_RESOURCE, HARU_KIND_NUM };

#define FUNCTION(id, names, params, fn) { HARU_STR_INIT(id), names, 2, params, sizeof params / sizeof params[0], sizeof params / sizeof params[0], fn, NULL }

static const HaruFunctionDesc counter_methods[] = {
    FUNCTION("add", add_names, resource_num_params, counter_add),
    FUNCTION("value", value_names, resource_param, counter_value),
};

static const HaruResourceDesc resources[] = {
    { HARU_STR_INIT("counter"), counter_names, 2, counter_drop, counter_methods, 2 },
};

static const HaruFunctionDesc functions[] = {
    FUNCTION("hello", hello_names, str_param, hello),
    FUNCTION("sum", sum_names, list_param, sum),
    FUNCTION("push_twice", push_twice_names, list_any_params, push_twice),
    FUNCTION("apply_twice", apply_twice_names, func_any_params, apply_twice),
    FUNCTION("counter", counter_names, num_param, counter_new),
    { HARU_STR_INIT("dropped"), dropped_names, 2, NULL, 0, 0, dropped, NULL },
};

static const HaruMessageDesc messages[] = {
    { HARU_STR_INIT("NotNumber"), HARU_STR_INIT("hari"), HARU_STR_INIT("{0}번째 원소가 숫자가 아니에요.") },
    { HARU_STR_INIT("NotNumber"), HARU_STR_INIT("kanade"), HARU_STR_INIT("{0}番目の要素が数ではありません。") },
};

static const HaruModuleDesc module = {
    HARU_ABI_VERSION,
    HARU_STR_INIT("greet_c"),
    module_names, 2,
    functions, sizeof functions / sizeof functions[0],
    messages, sizeof messages / sizeof messages[0],
    resources, sizeof resources / sizeof resources[0],
};

HARU_EXPORT const HaruModuleDesc *haru_module_v1_greet_c(const HaruHostApi *h) {
    host = h;
    counter_kind = &resources[0];
    return &module;
}
