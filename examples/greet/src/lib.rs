//! An example package. Built as a `cdylib` it is a dynamic module
//! (`haru_module_v1_greet`); as an `rlib` it links into a binary through
//! `greet::haru_entry`. The code is the same either way.

use std::cell::Cell;

use haru_sdk::prelude::*;

/// A resource: a counter the program holds as a value with methods.
struct Counter {
    n: Cell<f64>,
}

thread_local! {
    /// How many counters this thread has dropped (to watch resources being freed).
    static DROPPED: Cell<usize> = const { Cell::new(0) };
}

impl Drop for Counter {
    fn drop(&mut self) {
        DROPPED.with(|d| d.set(d.get() + 1));
    }
}

fn build(m: &mut Module) {
    m.name("hari", "인사").name("kanade", "挨拶");

    // `Str` reads the program's string in place (no copy).
    m.func("hello", |who: Str| format!("안녕, {who}!"))
        .name("hari", "인사말")
        .name("kanade", "挨拶文");

    // Lists are shared with the program: pushing here changes its list.
    m.func("sum", |list: List| -> Result<f64> {
        let mut total = 0.0;
        for (i, item) in list.iter().enumerate() {
            total += item.as_num().ok_or_else(|| Error::new("NotNumber").arg((i + 1) as f64))?;
        }
        Ok(total)
    })
    .name("hari", "합계")
    .name("kanade", "合計");

    m.func("push_twice", |list: List, item: Value| -> Result<()> {
        list.push(item.clone())?;
        list.push(item)
    })
    .name("hari", "두번추가")
    .name("kanade", "二回追加");

    // Callbacks: a function value from the program, called right here.
    m.func("apply_twice", |f: Func, x: Value| -> Result<Value> {
        let once = f.call(&[x])?;
        f.call(&[once])
    })
    .name("hari", "두번적용")
    .name("kanade", "二回適用");

    // `<계수기>(10)` makes one; `'c'의 <더하기>(5)` and `'c'의 <값>()` use it.
    let counter = m.resource::<Counter>("counter");
    counter.name("hari", "계수기").name("kanade", "カウンター");
    counter
        .method("add", |c: Res<Counter>, by: f64| {
            c.n.set(c.n.get() + by);
            c.n.get()
        })
        .name("hari", "더하기")
        .name("kanade", "足す");
    counter.method("value", |c: Res<Counter>| c.n.get()).name("hari", "값").name("kanade", "値");
    m.func("counter", |start: f64| Res::new(Counter { n: Cell::new(start) }))
        .name("hari", "계수기")
        .name("kanade", "カウンター");
    m.func("dropped", || DROPPED.with(Cell::get) as f64).name("hari", "해제된수").name("kanade", "解放数");

    m.message("NotNumber", "hari", "{0}번째 원소가 숫자가 아니에요.")
        .message("NotNumber", "kanade", "{0}番目の要素が数ではありません。");
}

haru_sdk::export!("greet", build);
