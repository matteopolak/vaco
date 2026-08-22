//! Assemble `vaco-registry` from per-crate `vaco-component.toml` fragments.
//!
//! Registration is generated rather than hand-edited because the registry would
//! otherwise be a file every one of ~120 crates must touch — the single worst
//! contention point in a shared working tree (plan 19 §3.4). A crate declares
//! what it provides; this walks the tree and emits the table.
//!
//! Linker-section registration (`inventory`, `linkme`) would also solve the
//! contention, but both rely on `unsafe` and link tricks, which D2 rules out.
//!
//! # What it emits
//!
//! Two artefacts, both inside `crates/registry/vaco-registry/`:
//!
//! 1. `src/generated.rs` — the component metadata table plus one typed
//!    descriptor table per kind, every entry `#[cfg(feature = …)]`-gated.
//! 2. A delimited region at the end of `Cargo.toml` holding the optional path
//!    dependency on each component crate and the `[features]` table.
//!
//! The manifest half matters more than it looks. Without it, registering a
//! component would still require a hand edit to a file shared by every
//! component author — the contention plan 19 §3.4 exists to remove, moved one
//! file along. Generating a **delimited region** rather than the whole manifest
//! keeps the hand-written half (package metadata, the always-on `-core`
//! dependencies, lints) reviewable and out of the generator's way, and the
//! generator only ever rewrites between its own markers.
//!
//! A generated path dependency that names a directory which does not exist
//! fails manifest parsing for the entire workspace (plan 19, the trap that has
//! blocked every agent five times). That cannot happen here by construction:
//! fragments are discovered by walking crate directories, so every crate named
//! in the output was found on disk a moment earlier.
//!
//! # Why a hand-written TOML reader
//!
//! `xtask` is dependency-free by design — it gates the build, so it must
//! compile before anything else and must not be able to violate the policies it
//! enforces. The fragment schema is frozen in plan 19 §3.4 and is a flat list of
//! `key = "string"` pairs under `[[component]]` headers, which is a small enough
//! language to read exactly rather than approximately. [`toml`] below is that
//! reader; it rejects what it does not understand instead of guessing, so a
//! malformed fragment is a named error rather than a silently missing component.

use std::path::Path;

use crate::{Map, Set, Task, crates, repo_root};

/// Marker lines delimiting the generated region of the registry's manifest.
const MANIFEST_BEGIN: &str = "# BEGIN GENERATED — `cargo xtask gen-registry`. Do not edit by hand.";
const MANIFEST_END: &str = "# END GENERATED";

// ---------------------------------------------------------------- the schema

/// The `kind` vocabulary, frozen in plan 19 §3.4.
///
/// The second column is the descriptor type a `ctor` of that kind must name.
/// `None` means the trait layer has no descriptor type for the kind yet, so the
/// generator emits the metadata row and a resolution check for the `ctor` path,
/// but no typed table — see the module docs of the generated file.
const KINDS: &[(&str, Option<Kind>)] = &[
    ("demuxer", Some(Kind::Demuxer)),
    ("muxer", Some(Kind::Muxer)),
    ("decoder", Some(Kind::Decoder)),
    ("encoder", None),
    ("parser", None),
    ("filter", Some(Kind::Filter)),
    ("protocol", Some(Kind::Protocol)),
    ("bitstream_filter", None),
];

/// A kind that has a descriptor type, and therefore a typed table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Demuxer,
    Muxer,
    Decoder,
    Filter,
    Protocol,
}

impl Kind {
    /// The Rust type of the descriptor a `ctor` of this kind names.
    const fn desc_ty(self) -> &'static str {
        match self {
            Self::Demuxer => "::vaco_format_core::DemuxerDesc",
            Self::Muxer => "::vaco_format_core::MuxerDesc",
            Self::Decoder => "::vaco_codec_core::DecoderDesc",
            Self::Filter => "::vaco_filter_core::FilterDesc",
            Self::Protocol => "::vaco_protocol_core::ProtocolDesc",
        }
    }

    /// The `static` the generated file exposes for this kind.
    const fn table(self) -> &'static str {
        match self {
            Self::Demuxer => "DEMUXERS",
            Self::Muxer => "MUXERS",
            Self::Decoder => "DECODERS",
            Self::Filter => "FILTERS",
            Self::Protocol => "PROTOCOLS",
        }
    }
}

/// Keys the schema defines. Anything else in a fragment is an error, so that a
/// typo becomes a message rather than a component that quietly does not exist.
const KEYS: &[&str] = &[
    "kind",
    "name",
    "long_name",
    "feature",
    "ctor",
    "media",
    "codec",
    "extensions",
    "mime_types",
    "default",
];

/// `media` vocabulary.
const MEDIA: &[&str] = &["video", "audio", "subtitle", "data"];

/// One `[[component]]` table, with its origin.
#[derive(Debug)]
struct Component {
    /// The crate that declared it, e.g. `vaco-demux-mp4`.
    krate: String,
    /// `crates/<area>` the crate lives under, for the generated path dep.
    area: String,
    kind: String,
    name: String,
    long_name: Option<String>,
    /// The cargo feature gating it; `None` for an always-on component.
    feature: Option<String>,
    ctor: String,
    media: Option<String>,
    codec: Option<String>,
    extensions: Vec<String>,
    mime_types: Vec<String>,
    /// Whether the feature is in `default`. Defaults to `true`; a component
    /// that must not ship in a default build (D4 — patent-encumbered) sets
    /// `default = false`.
    default_on: bool,
}

impl Component {
    /// The `#[cfg]` attribute line, empty for an always-on component.
    fn cfg(&self) -> String {
        match &self.feature {
            None => String::new(),
            Some(f) => format!("#[cfg(feature = {f:?})]\n"),
        }
    }
}

// ------------------------------------------------------------------- the task

pub fn run(check: bool) -> Task {
    let root = repo_root();
    let mut components: Vec<Component> = Vec::new();

    for (area, name, path) in crates() {
        let frag = path.join("vaco-component.toml");
        if !frag.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&frag).map_err(|e| format!("{name}: {e}"))?;
        let tables = toml::components(&text).map_err(|e| format!("{}: {e}", frag.display()))?;
        if tables.is_empty() {
            return Err(format!(
                "{}: no [[component]] table. A crate that registers nothing \
                 ships no fragment (plan 19 §3.4).",
                frag.display()
            ));
        }
        for t in tables {
            components.push(build(&t, &name, &area).map_err(|e| {
                format!("{}: [[component]] {}: {e}", frag.display(), t.origin_line)
            })?);
        }
    }

    // Deterministic and independent of directory-walk order.
    components.sort_by(|a, b| {
        (kind_rank(&a.kind), &a.name, &a.krate).cmp(&(kind_rank(&b.kind), &b.name, &b.krate))
    });
    check_unique(&components)?;

    // Through `rustfmt`, for the same reason `gen-pixfmt` does it: without it
    // the committed file fails `cargo fmt --check`, and then *every* contributor
    // has a formatter that silently rewrites a generated file and a `--check`
    // gate that fails right after. Measured directly — one `cargo fmt -p
    // vaco-registry` was enough to make `gen-registry --check` report the file
    // stale.
    let source = rustfmt(&emit_source(&components))?;
    let manifest = emit_manifest(&root, &components)?;

    let src_dest = root.join("crates/registry/vaco-registry/src/generated.rs");
    let toml_dest = root.join("crates/registry/vaco-registry/Cargo.toml");

    if check {
        for (dest, want) in [(&src_dest, &source), (&toml_dest, &manifest)] {
            let have = std::fs::read_to_string(dest).unwrap_or_default();
            if have != *want {
                return Err(format!(
                    "{} is stale; run `cargo xtask gen-registry`",
                    dest.display()
                ));
            }
        }
        println!(
            "gen-registry --check: up to date ({} components)",
            components.len()
        );
    } else {
        write_if_changed(&src_dest, &source)?;
        write_if_changed(&toml_dest, &manifest)?;
        println!("gen-registry: wrote {} components", components.len());
    }
    Ok(())
}

/// Pipe the emitted Rust through `rustfmt`.
///
/// `rustfmt` is a pinned toolchain component, so this is not a new dependency;
/// `gen-pixfmt` takes the same route for the same reason. A generated file that
/// `cargo fmt` wants to rewrite is a file whose `--check` gate fails the moment
/// anyone formats the workspace.
fn rustfmt(text: &str) -> Result<String, String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let config = repo_root().join("rustfmt.toml");
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--config-path"])
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run rustfmt (a pinned toolchain component): {e}"))?;
    child
        .stdin
        .take()
        .ok_or("rustfmt stdin was not piped")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("writing to rustfmt: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("waiting for rustfmt: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rustfmt rejected the generated registry:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("rustfmt emitted non-UTF-8: {e}"))
}

/// Write only on a real change, so a concurrent agent running the generator
/// converges instead of churning the file's mtime (plan 19 §3.6).
fn write_if_changed(dest: &Path, text: &str) -> Task {
    if std::fs::read_to_string(dest).is_ok_and(|c| c == text) {
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, text).map_err(|e| format!("{}: {e}", dest.display()))
}

fn kind_rank(kind: &str) -> usize {
    KINDS
        .iter()
        .position(|(k, _)| *k == kind)
        .unwrap_or(usize::MAX)
}

fn kind_of(kind: &str) -> Option<Kind> {
    KINDS.iter().find(|(k, _)| *k == kind).and_then(|(_, t)| *t)
}

/// Validate one parsed table into a [`Component`].
fn build(t: &toml::Table, krate: &str, area: &str) -> Result<Component, String> {
    for k in t.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(format!(
                "unknown key `{k}`. The schema is frozen in plan 19 §3.4: {}",
                KEYS.join(", ")
            ));
        }
    }

    let req = |k: &str| -> Result<String, String> {
        t.get(k)
            .map(str::to_owned)
            .ok_or_else(|| format!("`{k}` is required"))
    };

    let kind = req("kind")?;
    if kind_rank(&kind) == usize::MAX {
        let names: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
        return Err(format!(
            "unknown kind `{kind}`; the vocabulary is {}",
            names.join(", ")
        ));
    }
    let name = req("name")?;
    if name.is_empty() {
        return Err("`name` is empty".into());
    }
    let ctor = req("ctor")?;

    // `ctor` must resolve under the declaring crate's own name (plan 19 §3.4).
    let krate_path = krate.replace('-', "_");
    let head = ctor.split("::").next().unwrap_or_default();
    if head != krate_path {
        return Err(format!(
            "`ctor` is `{ctor}`, which does not start with `{krate_path}`. \
             A component's descriptor must live in the crate that declares it."
        ));
    }
    if !ctor.contains("::") {
        return Err(format!("`ctor` is `{ctor}`, which names no item"));
    }

    if let Some(m) = t.get("media")
        && !MEDIA.contains(&m)
    {
        return Err(format!(
            "`media = {m:?}` is not one of {}",
            MEDIA.join(", ")
        ));
    }

    let default_on = match t.get("default") {
        None => true,
        Some("true") => true,
        Some("false") => false,
        Some(other) => return Err(format!("`default` must be a boolean, got {other:?}")),
    };
    if !default_on && t.get("feature").is_none() {
        return Err(
            "`default = false` needs a `feature`; an always-on component is in \
             every build by definition"
                .into(),
        );
    }

    Ok(Component {
        krate: krate.to_owned(),
        area: area.to_owned(),
        kind,
        name,
        long_name: t.get("long_name").map(str::to_owned),
        feature: t.get("feature").map(str::to_owned),
        ctor,
        media: t.get("media").map(str::to_owned),
        codec: t.get("codec").map(str::to_owned),
        extensions: list(t.get("extensions")),
        mime_types: list(t.get("mime_types")),
        default_on,
    })
}

/// The schema spells lists as one comma-separated string, as the reference does
/// for `AVInputFormat::extensions`.
fn list(v: Option<&str>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Two components of the same kind answering to the same name would make
/// lookup order decide which one wins, which is exactly the sort of thing that
/// is invisible until it is a bug in someone else's crate.
fn check_unique(components: &[Component]) -> Task {
    let mut seen: Map<(String, String), &str> = Map::new();
    for c in components {
        // A descriptor `name` may be a comma-separated family; every element is
        // a valid spelling, so every element must be unique.
        for alias in c.name.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let key = (c.kind.clone(), alias.to_owned());
            if let Some(first) = seen.insert(key, &c.krate)
                && first != c.krate
            {
                return Err(format!(
                    "two crates register the {} named `{alias}`: {first} and {}",
                    c.kind, c.krate
                ));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ source emission

fn emit_source(components: &[Component]) -> String {
    let mut out = String::new();
    out.push_str(
        "//! GENERATED by `cargo xtask gen-registry`. Do not edit.\n\
         //!\n\
         //! Assembled from `vaco-component.toml` fragments so that adding a component\n\
         //! touches only that component's own crate (plan 19 §3.4).\n\
         //!\n\
         //! Every row is gated on the cargo feature its fragment names, so a disabled\n\
         //! component contributes no table entry, no dependency edge and no code.\n\
         //!\n\
         //! Kinds with no descriptor type in the trait layer yet — `encoder`, `parser`,\n\
         //! `bitstream_filter` — get a metadata row and a path-resolution check, but no\n\
         //! typed table. When `EncoderDesc` and friends land, add them to `KINDS` in\n\
         //! `xtask/src/registry.rs` and the tables appear.\n\n",
    );

    // -- metadata table -----------------------------------------------------
    out.push_str(
        "/// Every enabled component, ordered by (kind, name, crate).\n\
         ///\n\
         /// This is the listing surface: `-formats`, `-codecs`, `-demuxers` and the\n\
         /// rest render exactly these rows.\n\
         pub static COMPONENTS: &[crate::Component] = &[",
    );
    let mut rows = String::new();
    for c in components {
        rows.push_str(&indent_cfg(c));
        rows.push_str("    crate::Component {\n");
        rows.push_str(&format!(
            "        kind: crate::Kind::{},\n",
            variant(&c.kind)
        ));
        rows.push_str(&format!("        name: {:?},\n", c.name));
        rows.push_str(&format!(
            "        long_name: {},\n",
            opt_str(c.long_name.as_deref())
        ));
        rows.push_str(&format!("        krate: {:?},\n", c.krate));
        rows.push_str(&format!(
            "        feature: {},\n",
            opt_str(c.feature.as_deref())
        ));
        rows.push_str(&format!(
            "        media: {},\n",
            opt_str(c.media.as_deref())
        ));
        rows.push_str(&format!(
            "        codec: {},\n",
            opt_str(c.codec.as_deref())
        ));
        rows.push_str(&format!(
            "        extensions: &{:?},\n",
            c.extensions.as_slice()
        ));
        rows.push_str(&format!(
            "        mime_types: &{:?},\n",
            c.mime_types.as_slice()
        ));
        rows.push_str("    },\n");
    }
    close_slice(&mut out, &rows);

    // -- typed tables -------------------------------------------------------
    let mut kinds: Vec<Kind> = KINDS.iter().filter_map(|(_, k)| *k).collect();
    kinds.dedup();
    for kind in kinds {
        let rows: Vec<&Component> = components
            .iter()
            .filter(|c| kind_of(&c.kind) == Some(kind))
            .collect();
        out.push_str(&format!(
            "\n/// Descriptors of every enabled {} implementation.\n\
             pub static {}: &[&{}] = &[",
            KINDS
                .iter()
                .find(|(_, k)| *k == Some(kind))
                .map_or("", |(n, _)| *n),
            kind.table(),
            kind.desc_ty(),
        ));
        let mut body = String::new();
        for c in rows {
            body.push_str(&indent_cfg(c));
            body.push_str(&format!("    &::{},\n", c.ctor));
        }
        close_slice(&mut out, &body);
    }

    // -- resolution checks for kinds without a descriptor type --------------
    let unchecked: Vec<&Component> = components
        .iter()
        .filter(|c| kind_of(&c.kind).is_none())
        .collect();
    if !unchecked.is_empty() {
        out.push_str(
            "\n// Kinds with no descriptor type yet still get their `ctor` path checked,\n\
             // so a typo in a fragment is a compile error rather than a component that\n\
             // silently is not there. Taking a reference needs no trait bound, which is\n\
             // what makes this work without knowing the type.\n\
             const _: () = {\n",
        );
        for c in &unchecked {
            out.push_str(&indent_cfg(c));
            out.push_str(&format!("    let _ = &::{};\n", c.ctor));
        }
        out.push_str("};\n");
    }

    out
}

/// The component's `#[cfg]` line at one level of indentation, or nothing.
fn indent_cfg(c: &Component) -> String {
    let cfg = c.cfg();
    if cfg.is_empty() {
        String::new()
    } else {
        format!("    {}", cfg)
    }
}

/// Close a slice literal whose opening `&[` is already written.
///
/// An empty table has to close as `&[];` on one line, because that is what
/// `rustfmt` produces and the generated file is checked by `cargo fmt`. Getting
/// this wrong is invisible until the registry is empty, which is exactly the
/// state a `--no-default-features` build is in.
fn close_slice(out: &mut String, rows: &str) {
    if rows.is_empty() {
        out.push_str("];\n");
    } else {
        out.push('\n');
        out.push_str(rows);
        out.push_str("];\n");
    }
}

fn variant(kind: &str) -> String {
    kind.split('_')
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                None => String::new(),
                Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
            }
        })
        .collect()
}

fn opt_str(v: Option<&str>) -> String {
    v.map_or_else(|| "None".to_owned(), |s| format!("Some({s:?})"))
}

// ---------------------------------------------------------- manifest emission

fn emit_manifest(root: &Path, components: &[Component]) -> Result<String, String> {
    let dest = root.join("crates/registry/vaco-registry/Cargo.toml");
    let current = std::fs::read_to_string(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;

    let head = match current.split_once(MANIFEST_BEGIN) {
        None => current.trim_end().to_owned(),
        Some((before, _)) => before.trim_end().to_owned(),
    };
    Ok(head + &manifest_region(components))
}

/// The generated region, as a pure function of the component set.
fn manifest_region(components: &[Component]) -> String {
    // feature -> the crates it enables, and whether it is a default feature.
    let mut features: Map<String, Set<String>> = Map::new();
    let mut non_default: Set<String> = Set::new();
    let mut deps: Map<String, (String, bool)> = Map::new();
    for c in components {
        let optional = c.feature.is_some();
        let entry = deps
            .entry(c.krate.clone())
            .or_insert_with(|| (c.area.clone(), optional));
        // An always-on component in a crate makes the whole dependency
        // non-optional, whatever its other components say.
        entry.1 = entry.1 && optional;
        if let Some(f) = &c.feature {
            features
                .entry(f.clone())
                .or_default()
                .insert(c.krate.clone());
            if !c.default_on {
                non_default.insert(f.clone());
            }
        }
    }

    let mut out = String::from("\n\n");
    out.push_str(MANIFEST_BEGIN);
    out.push('\n');
    out.push_str(
        "#\n\
         # One optional path dependency per component crate, and one feature per\n\
         # `feature = …` a fragment names. `default` lists every feature that did not\n\
         # opt out with `default = false`, so a component can never be silently absent\n\
         # from a default build — the same rule `gen-fuzz` applies to its targets.\n\
         # A component that must not ship by default (D4, patent-encumbered) sets\n\
         # `default = false` in its own fragment; nothing here is hand-maintained.\n",
    );

    out.push_str("\n[features]\n");
    let default_list: Vec<String> = features
        .keys()
        .filter(|f| !non_default.contains(*f))
        .map(|f| format!("{f:?}"))
        .collect();
    if default_list.is_empty() {
        out.push_str("default = []\n");
    } else {
        out.push_str(&format!("default = [{}]\n", default_list.join(", ")));
    }
    for (feature, krates) in &features {
        let enables: Vec<String> = krates.iter().map(|k| format!("\"dep:{k}\"")).collect();
        out.push_str(&format!("{feature:?} = [{}]\n", enables.join(", ")));
    }

    for (krate, (area, optional)) in &deps {
        out.push_str(&format!("\n[dependencies.{krate}]\n"));
        out.push_str(&format!("path = \"../../{area}/{krate}\"\n"));
        if *optional {
            out.push_str("optional = true\n");
        }
    }

    out.push('\n');
    out.push_str(MANIFEST_END);
    out.push('\n');
    out
}

// -------------------------------------------------------------- a TOML reader

/// Just enough TOML for the frozen fragment schema, and no more.
///
/// Reads a file of `[[component]]` array-of-table headers each followed by
/// `key = "value"` pairs. Bare `true`/`false` are accepted as values so that
/// `default = false` reads naturally. Everything else — nested tables, arrays,
/// numbers, multi-line strings, dotted keys — is a **rejection**, not a
/// best-effort parse, because a fragment this reader half-understands would
/// register the wrong thing rather than nothing.
mod toml {
    use crate::Map;

    /// One `[[component]]` table: its keys, and the line it started on.
    #[derive(Debug, Default)]
    pub struct Table {
        map: Map<String, String>,
        pub origin_line: usize,
    }

    impl Table {
        pub fn get(&self, key: &str) -> Option<&str> {
            self.map.get(key).map(String::as_str)
        }

        pub fn keys(&self) -> impl Iterator<Item = &String> {
            self.map.keys()
        }
    }

    /// Parse every `[[component]]` table in `text`.
    ///
    /// # Errors
    /// A message naming the line, for anything outside the schema's grammar.
    pub fn components(text: &str) -> Result<Vec<Table>, String> {
        let mut tables: Vec<Table> = Vec::new();
        let mut open = false;

        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            let s = strip_comment(raw).trim();
            if s.is_empty() {
                continue;
            }

            if let Some(rest) = s.strip_prefix("[[") {
                let name = rest
                    .strip_suffix("]]")
                    .ok_or_else(|| format!("line {line}: unterminated `[[` header"))?
                    .trim();
                if name != "component" {
                    return Err(format!(
                        "line {line}: `[[{name}]]` — the only table this schema \
                         defines is `[[component]]`"
                    ));
                }
                tables.push(Table {
                    map: Map::new(),
                    origin_line: line,
                });
                open = true;
                continue;
            }
            if s.starts_with('[') {
                return Err(format!(
                    "line {line}: `{s}` — a fragment holds only `[[component]]` \
                     tables (plan 19 §3.4)"
                ));
            }

            let (key, value) = s
                .split_once('=')
                .ok_or_else(|| format!("line {line}: `{s}` is not `key = value`"))?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("line {line}: `{key}` is not a bare key"));
            }
            if !open {
                return Err(format!(
                    "line {line}: `{key}` appears before any `[[component]]` header"
                ));
            }
            let value = scalar(value.trim(), line)?;

            let Some(t) = tables.last_mut() else {
                return Err(format!("line {line}: no open table"));
            };
            if t.map.insert(key.to_owned(), value).is_some() {
                return Err(format!("line {line}: `{key}` is set twice"));
            }
        }
        Ok(tables)
    }

    /// Remove a trailing `#` comment, respecting a quoted `#`.
    fn strip_comment(s: &str) -> &str {
        let mut in_str = false;
        let mut escaped = false;
        for (i, c) in s.char_indices() {
            if escaped {
                escaped = false;
            } else if in_str && c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = !in_str;
            } else if c == '#' && !in_str {
                return s.get(..i).unwrap_or(s);
            }
        }
        s
    }

    /// A basic string, or a bare `true`/`false`.
    fn scalar(s: &str, line: usize) -> Result<String, String> {
        if s == "true" || s == "false" {
            return Ok(s.to_owned());
        }
        let inner = s
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .ok_or_else(|| {
                format!(
                    "line {line}: {s:?} — values are double-quoted strings, or \
                     `true`/`false`"
                )
            })?;

        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                if c == '"' {
                    return Err(format!("line {line}: unescaped `\"` inside a string"));
                }
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    return Err(format!("line {line}: unsupported escape `\\{other}`"));
                }
                None => return Err(format!("line {line}: string ends in a backslash")),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(text: &str) -> Result<Component, String> {
        let tables = toml::components(text)?;
        let t = tables.first().ok_or("no table")?;
        build(t, "vaco-demux-mp4", "format")
    }

    const MP4: &str = r#"
        # a comment
        [[component]]
        kind      = "demuxer"
        name      = "mov,mp4,m4a,3gp,3g2,mj2"   # aliases are one component
        long_name = "QuickTime / MOV"
        feature   = "demux-mp4"
        ctor      = "vaco_demux_mp4::MP4_DEMUXER"
        extensions = "mp4,m4a,mov"
        mime_types = "video/mp4"
    "#;

    #[test]
    fn reads_the_frozen_schema() {
        let c = parse_one(MP4).expect("parse");
        assert_eq!(c.kind, "demuxer");
        assert_eq!(c.name, "mov,mp4,m4a,3gp,3g2,mj2");
        assert_eq!(c.long_name.as_deref(), Some("QuickTime / MOV"));
        assert_eq!(c.feature.as_deref(), Some("demux-mp4"));
        assert_eq!(c.extensions, ["mp4", "m4a", "mov"]);
        assert_eq!(c.mime_types, ["video/mp4"]);
        assert!(c.default_on);
    }

    #[test]
    fn a_comment_containing_a_quote_is_still_a_comment() {
        let c = parse_one(
            r#"[[component]]
               kind = "demuxer"
               name = "x" # it's "quoted"
               ctor = "vaco_demux_mp4::D"
            "#,
        )
        .expect("parse");
        assert_eq!(c.name, "x");
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let c = parse_one(
            r#"[[component]]
               kind = "demuxer"
               name = "a#b"
               ctor = "vaco_demux_mp4::D"
            "#,
        )
        .expect("parse");
        assert_eq!(c.name, "a#b");
    }

    #[test]
    fn several_tables_in_one_file() {
        let text = r#"
            [[component]]
            kind = "demuxer"
            name = "a"
            ctor = "vaco_demux_mp4::A"

            [[component]]
            kind = "muxer"
            name = "b"
            ctor = "vaco_demux_mp4::B"
        "#;
        let tables = toml::components(text).expect("parse");
        assert_eq!(tables.len(), 2);
        assert_eq!(tables.get(1).and_then(|t| t.get("kind")), Some("muxer"));
    }

    #[test]
    fn rejections() {
        for (text, why) in [
            (
                r#"[[component]]
                   kind = "demuxer"
                   ctor = "vaco_demux_mp4::D""#,
                "missing name",
            ),
            (
                r#"[[component]]
                   kind = "nonesuch"
                   name = "a"
                   ctor = "vaco_demux_mp4::D""#,
                "unknown kind",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   ctor = "vaco_other::D""#,
                "ctor outside the declaring crate",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   ctor = "vaco_demux_mp4""#,
                "ctor names no item",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   ctor = "vaco_demux_mp4::D"
                   colour = "blue""#,
                "unknown key",
            ),
            (
                r#"[[component]]
                   kind = "decoder"
                   name = "a"
                   media = "smell"
                   ctor = "vaco_demux_mp4::D""#,
                "unknown media",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   default = false
                   ctor = "vaco_demux_mp4::D""#,
                "default = false without a feature",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   name = "b"
                   ctor = "vaco_demux_mp4::D""#,
                "duplicate key",
            ),
            (
                r#"[dependencies]
                   kind = "demuxer""#,
                "not a [[component]] table",
            ),
            (r#"kind = "demuxer""#, "key before any header"),
            (
                r#"[[component]]
                   kind = demuxer"#,
                "unquoted value",
            ),
            (
                r#"[[component]]
                   kind = "demuxer"
                   name = "a"
                   ctor = "vaco_demux_mp4::D"
                   extensions = ["mp4"]"#,
                "an array is not the schema's list spelling",
            ),
        ] {
            assert!(parse_one(text).is_err(), "should have rejected: {why}");
        }
    }

    #[test]
    fn escapes_round_trip() {
        let c = parse_one(
            r#"[[component]]
               kind = "demuxer"
               name = "a"
               long_name = "say \"hi\"\tnow"
               ctor = "vaco_demux_mp4::D""#,
        )
        .expect("parse");
        assert_eq!(c.long_name.as_deref(), Some("say \"hi\"\tnow"));
    }

    #[test]
    fn duplicate_names_within_a_kind_are_rejected() {
        let mk = |krate: &str, name: &str| Component {
            krate: krate.to_owned(),
            area: "format".to_owned(),
            kind: "demuxer".to_owned(),
            name: name.to_owned(),
            long_name: None,
            feature: None,
            ctor: "x::Y".to_owned(),
            media: None,
            codec: None,
            extensions: Vec::new(),
            mime_types: Vec::new(),
            default_on: true,
        };
        // An alias collision counts: `-f mp4` must select exactly one demuxer.
        assert!(check_unique(&[mk("a", "mov,mp4"), mk("b", "mp4")]).is_err());
        assert!(check_unique(&[mk("a", "mov,mp4"), mk("b", "mkv")]).is_ok());
        // Same crate declaring the same name twice is its own problem, and the
        // uniqueness check is about cross-crate collisions.
        assert!(check_unique(&[mk("a", "x"), mk("a", "x")]).is_ok());
    }

    #[test]
    fn the_generated_source_gates_every_featured_row() {
        let c = parse_one(MP4).expect("parse");
        let src = emit_source(&[c]);
        assert!(src.contains("#[cfg(feature = \"demux-mp4\")]"));
        assert!(src.contains("&::vaco_demux_mp4::MP4_DEMUXER,"));
        assert!(src.contains("pub static DEMUXERS: &[&::vaco_format_core::DemuxerDesc]"));
        assert!(src.contains("kind: crate::Kind::Demuxer,"));
        // Every typed table exists even when empty, so the registry's own source
        // never has to `#[cfg]` around a missing name.
        for t in ["MUXERS", "DECODERS", "FILTERS", "PROTOCOLS"] {
            assert!(src.contains(t), "{t}");
        }
    }

    #[test]
    fn an_unkinded_ctor_is_still_resolution_checked() {
        let text = r#"[[component]]
            kind = "encoder"
            name = "e"
            ctor = "vaco_demux_mp4::ENC""#;
        let tables = toml::components(text).expect("parse");
        let t = tables.first().expect("one table");
        let c = build(t, "vaco-demux-mp4", "format").expect("build");
        let src = emit_source(&[c]);
        assert!(src.contains("let _ = &::vaco_demux_mp4::ENC;"));
        assert!(!src.contains("ENC,\n"), "no typed table for `encoder` yet");
    }

    /// A component in `krate` under `crates/<area>`, with `feature`.
    fn comp(krate: &str, area: &str, feature: Option<&str>, default_on: bool) -> Component {
        Component {
            krate: krate.to_owned(),
            area: area.to_owned(),
            kind: "demuxer".to_owned(),
            name: krate.to_owned(),
            long_name: None,
            feature: feature.map(str::to_owned),
            ctor: format!("{}::D", krate.replace('-', "_")),
            media: None,
            codec: None,
            extensions: Vec::new(),
            mime_types: Vec::new(),
            default_on,
        }
    }

    #[test]
    fn the_manifest_region_gates_and_defaults() {
        let region = manifest_region(&[
            comp("vaco-demux-mp4", "format", Some("demux-mp4"), true),
            comp("vaco-codec-h265", "codec", Some("codec-h265"), false),
        ]);
        // Every feature is in `default` unless the fragment opted out (D4).
        assert!(region.contains("default = [\"demux-mp4\"]"), "{region}");
        assert!(region.contains("\"codec-h265\" = [\"dep:vaco-codec-h265\"]"));
        assert!(region.contains("[dependencies.vaco-demux-mp4]"));
        assert!(region.contains("path = \"../../format/vaco-demux-mp4\""));
        // A featured component's dependency edge must be optional, or
        // `--no-default-features` still compiles the component crate.
        assert_eq!(region.matches("optional = true").count(), 2);
        assert!(region.starts_with("\n\n# BEGIN GENERATED"));
        assert!(region.ends_with("# END GENERATED\n"));
    }

    #[test]
    fn an_always_on_component_is_a_non_optional_dependency() {
        let region = manifest_region(&[comp("vaco-demux-raw", "format", None, true)]);
        assert!(region.contains("[dependencies.vaco-demux-raw]"));
        assert!(!region.contains("optional = true"));
        assert!(region.contains("default = []"));
    }

    #[test]
    fn one_crate_with_a_featured_and_an_always_on_component_is_not_optional() {
        // The dependency edge has to satisfy the *strictest* component in the
        // crate: if any one of them is always on, the crate is always compiled.
        let region = manifest_region(&[
            comp("vaco-demux-x", "format", Some("f"), true),
            Component {
                name: "y".to_owned(),
                ..comp("vaco-demux-x", "format", None, true)
            },
        ]);
        assert!(!region.contains("optional = true"), "{region}");
    }

    #[test]
    fn two_crates_may_share_one_feature() {
        let region = manifest_region(&[
            comp("vaco-a", "format", Some("shared"), true),
            comp("vaco-b", "format", Some("shared"), true),
        ]);
        assert!(
            region.contains("\"shared\" = [\"dep:vaco-a\", \"dep:vaco-b\"]"),
            "{region}"
        );
        assert!(region.contains("default = [\"shared\"]"));
    }

    #[test]
    fn generating_the_region_is_idempotent() {
        let one = manifest_region(&[comp("vaco-a", "format", Some("f"), true)]);
        let two = manifest_region(&[comp("vaco-a", "format", Some("f"), true)]);
        assert_eq!(one, two);
    }

    #[test]
    fn emitting_twice_is_identical() {
        let a = parse_one(MP4).expect("parse");
        let b = parse_one(MP4).expect("parse");
        assert_eq!(emit_source(&[a]), emit_source(&[b]));
    }
}
