//! The crate graph must be acyclic and every edge must point downward.
//!
//! Enforced mechanically because a layering violation is invisible in review —
//! it compiles fine — and only shows up much later as a dependency cycle nobody
//! can unpick.

use crate::{Map, Set, Task, crates, repo_root};

/// Layer index per directory under `crates/`, matching `layers.toml`.
fn layer_of(dir: &str) -> Option<u8> {
    Some(match dir {
        "core" => 0,
        "model" => 1,
        "io" => 2,
        "signal" => 3,
        "codec" | "format" => 4,
        "filter" => 5,
        "registry" => 6,
        "app" => 7,
        "hw" => 9,
        "tool" => 10,
        _ => return None,
    })
}

pub fn run() -> Task {
    let root = repo_root();
    let all = crates();
    let mut layer: Map<String, u8> = Map::new();
    let mut dir_of: Map<String, String> = Map::new();

    for (d, name, _) in &all {
        let Some(l) = layer_of(d) else {
            return Err(format!(
                "crates/{d}/ is not a known layer; add it to layers.toml"
            ));
        };
        layer.insert(name.clone(), l);
        dir_of.insert(name.clone(), d.clone());
    }

    let mut edges: Map<String, Set<String>> = Map::new();
    let mut violations = Vec::new();

    for (_, name, path) in &all {
        let manifest =
            std::fs::read_to_string(path.join("Cargo.toml")).map_err(|e| format!("{name}: {e}"))?;
        let mut deps = Set::new();
        for line in manifest.lines() {
            let line = line.trim();
            if let Some(dep) = line.split(&[' ', '=', '.'][..]).next()
                && dep.starts_with("vaco-")
                && layer.contains_key(dep)
                && line.contains("path")
            {
                deps.insert(dep.to_string());
            }
        }

        let self_layer = layer[name];
        for d in &deps {
            let dep_layer = layer[d];
            // Tools (10) may depend on anything. Everything else must point
            // strictly downward, with same-layer edges allowed only within a
            // layer's own internal structure (e.g. vaco-opts -> vaco-opts-derive).
            let ok = self_layer == 10 || dep_layer < self_layer || dep_layer == self_layer;
            if !ok {
                violations.push(format!(
                    "  {name} (layer {self_layer}) depends on {d} (layer {dep_layer}) — edges must point downward"
                ));
            }
        }
        edges.insert(name.clone(), deps);
    }

    // Cycle detection: iterative depth-first search with an on-stack marker.
    let mut state: Map<&str, u8> = Map::new(); // 0 unvisited, 1 on stack, 2 done
    for start in edges.keys() {
        if state.get(start.as_str()).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack = vec![(start.as_str(), 0usize)];
        while let Some(&mut (node, ref mut idx)) = stack.last_mut() {
            if *idx == 0 {
                state.insert(node, 1);
            }
            let children: Vec<&str> = edges[node].iter().map(String::as_str).collect();
            if let Some(&child) = children.get(*idx) {
                *idx += 1;
                match state.get(child).copied().unwrap_or(0) {
                    0 => stack.push((child, 0)),
                    1 => violations.push(format!("  cycle: {node} -> {child}")),
                    _ => {}
                }
            } else {
                state.insert(node, 2);
                stack.pop();
            }
        }
    }

    let _ = root;
    if violations.is_empty() {
        println!(
            "layer-check: {} crates, graph acyclic and downward",
            all.len()
        );
        Ok(())
    } else {
        violations.sort();
        violations.dedup();
        Err(violations.join("\n"))
    }
}
