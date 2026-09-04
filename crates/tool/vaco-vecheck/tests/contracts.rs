#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a failing assertion in a test is a failing test"
)]

use vaco_vecheck::{Config, Date, verify_assembly, verify_remarks};

const CONFIG: &str = r#"
max_live_waiver_cost_pct = 3.0

[[kernel]]
id = "demo/vector_add"
variant = "x8"
symbol = "demo::vector_add"
package = "demo"

[kernel.expect.x86_64-v3]
require = ["vaddps", "vmulps"]
forbid = ["\\bcall\\b", "\\bud2\\b"]
max_insns = 4
"#;

const PASSED: &str = r#"
--- !Passed
Pass: loop-vectorize
Name: Vectorized
Function: "demo::vector_add::h123"
Args:
  - String: "vectorized loop (vectorization width: 8)"
...
"#;

const MISSED: &str = r#"
--- !Missed
Pass: loop-vectorize
Name: MissedDetails
Function: "demo::vector_add::h123"
Args:
  - String: "loop not vectorized: unsafe dependent memory operations"
...
"#;

#[test]
fn passed_loop_vectorize_remark_satisfies_a_configured_symbol() {
    let config = Config::parse(CONFIG).expect("config parses");
    verify_remarks(&config, PASSED).expect("passed remark satisfies the contract");
}

#[test]
fn quoted_symbols_may_contain_a_hash() {
    let config = Config::parse(
        r#"
[[kernel]]
id = "demo/closure"
variant = "x8"
symbol = "demo::function"
asm_symbol = "demo::{closure#1}"
package = "demo"
"#,
    )
    .expect("hash within a quoted selector is not a comment");
    assert!(config.kernel("demo/closure").is_some());
}

#[test]
fn missed_loop_vectorize_remark_fails_with_llvm_reason() {
    let config = Config::parse(CONFIG).expect("config parses");
    let error = verify_remarks(&config, MISSED).expect_err("missed remark must fail");
    assert!(
        error
            .to_string()
            .contains("unsafe dependent memory operations")
    );
    assert!(error.to_string().contains("demo/vector_add"));
}

#[test]
fn assembly_assertions_require_forbid_and_count_instructions() {
    let config = Config::parse(CONFIG).expect("config parses");
    let assembly = ".Lhot:\nvaddps ymm0, ymm1, ymm2\nvmulps ymm0, ymm0, ymm3\njne .Lhot\nret\n";
    verify_assembly(&config, "x86_64-v3", "demo/vector_add", assembly)
        .expect("assembly meets the contract");

    let error = verify_assembly(
        &config,
        "x86_64-v3",
        "demo/vector_add",
        "vaddps ymm0, ymm1, ymm2\nvmulps ymm0, ymm0, ymm3\ncall qword ptr [rax]\nret\n",
    )
    .expect_err("outlined call must fail");
    assert!(error.to_string().contains("forbidden"));
}

#[test]
fn assembly_alternatives_accept_a_target_specific_narrowing_opcode() {
    let config = Config::parse(
        r#"
[[kernel]]
id = "demo/narrow"
variant = "x16"
symbol = "demo::narrow"
package = "demo"

[kernel.expect.x86_64-v3]
require = ["vpackusdw|vpmovusdw|vpmovdw"]
"#,
    )
    .expect("config parses");
    verify_assembly(
        &config,
        "x86_64-v3",
        "demo/narrow",
        "vpmovusdw xmm0, ymm0\nret\n",
    )
    .expect("target-specific alternative meets the narrowing requirement");
}

#[test]
fn assembly_rejects_a_different_dispatched_isa_body() {
    let config = Config::parse(
        r#"
[[kernel]]
id = "demo/dispatched"
variant = "x16"
symbol = "demo::dispatched"
asm_symbol = "vectorize_avx2::<demo::dispatched::{closure#1}"
package = "demo"

[kernel.expect.x86_64-v3]
require = ["vpmaddwd"]
forbid = ["call"]
max_insns = 2
"#,
    )
    .expect("config parses");
    let avx512 = "<Avx512 as Simd>::vectorize::vectorize_avx512::<demo::dispatched::{closure#2}>:\n.Lloop:\nvpmaddwd ymm0, ymm1, ymm2\njne .Lloop\n";
    let error = verify_assembly(&config, "x86_64-v3", "demo/dispatched", avx512)
        .expect_err("an AVX-512 closure must not satisfy an AVX2 contract");
    assert!(error.to_string().contains("expected emitted symbol"));
    assert!(error.to_string().contains("vectorize_avx2"));
}

#[test]
fn instruction_budget_counts_only_the_unique_required_hot_loop() {
    let config = Config::parse(
        r#"
[[kernel]]
id = "demo/hot"
variant = "x8"
symbol = "demo::hot"
package = "demo"

[kernel.expect.x86_64-v3]
require = ["vaddps", "vmulps"]
forbid = ["\\bcall\\b"]
max_insns = 3
"#,
    )
    .expect("config parses");
    verify_assembly(
        &config,
        "x86_64-v3",
        "demo/hot",
        "push rbp\n.Lhot:\nvaddps ymm0, ymm1, ymm2\nvmulps ymm0, ymm0, ymm3\njne .Lhot\npop rbp\nret\n",
    )
    .expect("only the three-instruction hot loop is budgeted");
}

#[test]
fn ambiguous_matching_loops_fail_closed() {
    let config = Config::parse(
        r#"
[[kernel]]
id = "demo/ambiguous"
variant = "x8"
symbol = "demo::ambiguous"
package = "demo"

[kernel.expect.x86_64-v3]
require = ["vaddps"]
max_insns = 2
"#,
    )
    .expect("config parses");
    let error = verify_assembly(
        &config,
        "x86_64-v3",
        "demo/ambiguous",
        ".Lfirst:\nvaddps ymm0, ymm1, ymm2\njne .Lfirst\n.Lsecond:\nvaddps ymm0, ymm1, ymm2\njne .Lsecond\n",
    )
    .expect_err("two matching loops must not select arbitrarily");
    assert!(error.to_string().contains("2 backward-edge loops"));
}

#[test]
fn expired_waivers_and_excess_live_cost_fail_validation() {
    let config = Config::parse(
        r#"
max_live_waiver_cost_pct = 3.0

[[kernel]]
id = "demo/vector_add"
variant = "x16"
symbol = "demo::vector_add"
package = "demo"

[[kernel]]
id = "demo/other"
variant = "x16"
symbol = "demo::other"
package = "demo"

[[waiver]]
kernel = "demo/vector_add"
variant = "x16"
reason = "LLVM regression"
upstream = "https://github.com/rust-lang/rust/issues/1"
expires = "2026-01-01"
cost_pct = 2.0

[[waiver]]
kernel = "demo/other"
variant = "x16"
reason = "LLVM regression"
upstream = "https://github.com/rust-lang/rust/issues/2"
expires = "2026-12-01"
cost_pct = 2.0
"#,
    )
    .expect("config parses");

    let error = config
        .validate(Date::parse("2026-02-01").expect("date parses"))
        .expect_err("expired waiver must fail before its cost can be accepted");
    assert!(error.to_string().contains("expired"));

    let error = config
        .validate(Date::parse("2025-12-31").expect("date parses"))
        .expect_err("live waiver cost above the configured ceiling must fail");
    assert!(error.to_string().contains("4.000%"));
}
