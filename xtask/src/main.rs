//! Vaco developer tasks. Kept dependency-free on purpose: it gates the build, so
//! it must compile before anything else does.

fn main() {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "layer-check" => todo!("P0-04: assert the crate graph is acyclic and downward"),
        "dep-gate" => todo!("P0-04: fail on any `links` key or third-party build.rs (D10 Gate 1)"),
        "unsafe-audit" => todo!("P0-04: assert `unsafe` appears only where D2/D13 permit"),
        "patent-check" => {
            todo!("P0-04: assert patent-encumbered features absent from default (D4)")
        }
        "gen-registry" => todo!("P0-04: assemble vaco-registry from vaco-component.toml fragments"),
        "gen-docs-index" => todo!("P0-04: generate docs/README.md from per-crate front-matter"),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!(
                "tasks: layer-check dep-gate unsafe-audit patent-check gen-registry gen-docs-index"
            );
            std::process::exit(2);
        }
    }
}
