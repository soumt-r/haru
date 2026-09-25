# Haru

하리(Hari)·카나데(Kanade)의 Rust 런타임(연구용). 언어 규칙은 Hana와 같고, 런타임 구조는 성능과 패키지 개발 편의를 중심으로 새로 설계합니다 — [DESIGN.md](DESIGN.md).

## 지금 되는 것 (M0–M1)

- `crates/abi` — 네이티브 모듈 ABI v1 (`#[repr(C)]`, 의존성 0)
- `crates/sdk` — Rust 함수를 그대로 모듈 함수로 (`m.func("ceil", f64::ceil).name("hari", "올림")`)
- `crates/syntax` — 하리·카나데 렉서·파서. Hana와 트리·구문 진단이 같음을 `tools/parity.sh`로 확인 (`haru ast <파일>`)
- `crates/core` — 16바이트 값, 모듈 레지스트리, HostApi, 정적/동적 로드
- `std/` — SDK로 쓴 `[수학]` (바이너리에 정적 링크)
- `examples/greet` — 같은 코드가 `.dll`/`.so`로도, 정적 링크로도 되는 예제 패키지

```bash
cargo test
cargo run -q -- call 수학 올림 3.2
cargo run -q -- call --lang kanade 数学 階乗 5
```
