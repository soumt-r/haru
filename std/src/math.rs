//! [수학] / 【数学】 (ids and names follow Hana's `std/std.go`).

use haru_sdk::prelude::*;

haru_sdk::entry!(pub(crate) fn entry = "math", build);

fn build(m: &mut Module) {
    m.name("hari", "수학").name("kanade", "数学");

    m.func("ceil", f64::ceil).name("hari", "올림").name("kanade", "切り上げ");
    m.func("floor", f64::floor).name("hari", "버림").name("kanade", "切り捨て");
    m.func("abs", f64::abs).name("hari", "절댓값").name("kanade", "絶対値");
    m.func("round", f64::round).name("hari", "반올림").name("kanade", "四捨五入");
    m.func("pow", f64::powf).name("hari", "거듭제곱").name("kanade", "べき乗");
    m.func("pi", || std::f64::consts::PI).name("hari", "파이").name("kanade", "円周率");

    m.func("sqrt", |x: f64| -> Result<f64> {
        if x < 0.0 {
            return Err(Error::new("NegativeRoot").arg(x));
        }
        Ok(x.sqrt())
    })
    .name("hari", "제곱근")
    .name("kanade", "平方根");

    m.func("factorial", |n: f64| -> Result<f64> {
        if n < 0.0 || n.fract() != 0.0 {
            return Err(Error::new("FactorialDomain").arg(n));
        }
        Ok((1..=n as u64).fold(1.0, |acc, k| acc * k as f64))
    })
    .name("hari", "팩토리얼")
    .name("kanade", "階乗");

    m.message("NegativeRoot", "hari", "음수 {0}의 제곱근은 구할 수 없어요.")
        .message("NegativeRoot", "kanade", "負の数{0}の平方根は求められません。")
        .message("NegativeRoot", "en", "no square root of negative {0}")
        .message("FactorialDomain", "hari", "팩토리얼은 0 이상의 정수만 돼요: {0}")
        .message("FactorialDomain", "kanade", "階乗は0以上の整数だけです: {0}")
        .message("FactorialDomain", "en", "factorial needs a non-negative integer: {0}");
}
