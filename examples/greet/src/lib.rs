//! An example package. Built as a `cdylib` it is a dynamic module
//! (`haru_module_v1_greet`); as an `rlib` it links into a binary through
//! `greet::haru_entry`. The code is the same either way.

use haru_sdk::prelude::*;

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

    m.message("NotNumber", "hari", "{0}번째 원소가 숫자가 아니에요.")
        .message("NotNumber", "kanade", "{0}番目の要素が数ではありません。");
}

haru_sdk::export!("greet", build);
