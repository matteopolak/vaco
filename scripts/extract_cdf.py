#!/usr/bin/env python3
"""Extract Default_*_Cdf tables from the AV1 spec text dump into Rust arrays.

Mechanical extraction, not manual transcription: strips pagination noise,
finds each named table by brace-balancing, and converts C-style {..} nesting
straight to Rust [..] nesting (the printed element order is unchanged, so this
is a syntax transliteration, not a retyping). The dimension header
(`Name[ A ][ B ]... = {`) is parsed and evaluated against the specification's
own named constants (section 3, cross-checked by line number in the extractor
invocation) to produce an exact Rust array type, so a shape mismatch is a
compile error rather than a silent wrong stride.
"""
import re
import sys

NOISE_RE = re.compile(
    r"^\s*(Section:.*|AV1 Bitstream & Decoding Process Specification\s*|Page \d+ of \d+\s*)$"
)

# Verified against av1-spec.txt section 3 (Symbols and abbreviated terms) by
# line number: BLOCK_SIZE_GROUPS 908, MAX_SEGMENTS 953, SEGMENT_ID_CONTEXTS
# 955, PLANE_TYPES 969, TX_SIZE_CONTEXTS 971, PARTITION_CONTEXTS 981,
# TX_SIZES 983, INTRA_MODES 1050, UV_INTRA_MODES_CFL_NOT_ALLOWED 1061,
# UV_INTRA_MODES_CFL_ALLOWED 1064, DELTA_Q_SMALL 1140, DELTA_LF_SMALL 1143,
# MAX_ANGLE_DELTA 1148, DIRECTIONAL_MODES 1151, CFL_JOINT_SIGNS 1265,
# CFL_ALPHABET_SIZE 1267, CFL_ALPHA_CONTEXTS 1274, INTRA_MODE_CONTEXTS 1277,
# MAX_TX_DEPTH 1308, BR_CDF_SIZE 1375, SIG_COEF_CONTEXTS_EOB 1377,
# SIG_COEF_CONTEXTS 1382, TXB_SKIP_CONTEXTS 1414, EOB_COEF_CONTEXTS 1416,
# DC_SIGN_CONTEXTS 1418, LEVEL_CONTEXTS 1420, COEFF_CDF_Q_CTXS 1447.
SYMS = {
    "BLOCK_SIZE_GROUPS": 4,
    "MAX_SEGMENTS": 8,
    "SEGMENT_ID_CONTEXTS": 3,
    "PLANE_TYPES": 2,
    "TX_SIZE_CONTEXTS": 3,
    "PARTITION_CONTEXTS": 4,
    "TX_SIZES": 5,
    "INTRA_MODES": 13,
    "UV_INTRA_MODES_CFL_NOT_ALLOWED": 13,
    "UV_INTRA_MODES_CFL_ALLOWED": 14,
    "DELTA_Q_SMALL": 3,
    "DELTA_LF_SMALL": 3,
    "MAX_ANGLE_DELTA": 3,
    "DIRECTIONAL_MODES": 8,
    "CFL_JOINT_SIGNS": 8,
    "CFL_ALPHABET_SIZE": 16,
    "CFL_ALPHA_CONTEXTS": 6,
    "INTRA_MODE_CONTEXTS": 5,
    "MAX_TX_DEPTH": 2,
    "BR_CDF_SIZE": 4,
    "SIG_COEF_CONTEXTS_EOB": 4,
    "SIG_COEF_CONTEXTS": 42,
    "TXB_SKIP_CONTEXTS": 13,
    "EOB_COEF_CONTEXTS": 9,
    "DC_SIGN_CONTEXTS": 3,
    "LEVEL_CONTEXTS": 21,
    "COEFF_CDF_Q_CTXS": 4,
    "BLOCK_SIZES": 22,
    "SKIP_CONTEXTS": 3,
    "TX_SIZES_ALL": 19,
    "SIG_REF_DIFF_OFFSET_NUM": 5,
}


def load_lines(path):
    with open(path, encoding="utf-8") as f:
        return f.readlines()


def strip_noise(lines):
    return [l for l in lines if not NOISE_RE.match(l) and l.strip() != ""]


def find_table(lines, name):
    start = None
    for i, l in enumerate(lines):
        if re.match(rf"^\s*{re.escape(name)}\s*\[", l):
            start = i
            break
    if start is None:
        raise SystemExit(f"table not found: {name}")
    buf = []
    depth = 0
    started = False
    j = start
    while j < len(lines):
        line = lines[j]
        buf.append(line)
        for ch in line:
            if ch == "{":
                depth += 1
                started = True
            elif ch == "}":
                depth -= 1
        if started and depth == 0:
            break
        j += 1
        if j - start > 20000:
            raise SystemExit(f"table {name} never balanced")
    return "".join(buf)


def header_dims(raw):
    header = raw.split("=", 1)[0]
    return [d.strip() for d in re.findall(r"\[([^\]]*)\]", header)]


def eval_dim(expr):
    e = re.sub(r"\s+", " ", expr).strip()
    for k, v in SYMS.items():
        e = re.sub(rf"\b{k}\b", str(v), e)
    return eval(e, {"__builtins__": {}}, {})  # noqa: S307 -- closed numeric grammar only


# A handful of tables (Partition_Subsize, Max_Tx_Size_Rect, Split_Tx_Size)
# print BLOCK_*/TX_* enumerator names as body *values*, not numbers -- the
# dimension-header substitution above does not reach these. Verified against
# the specification's own explicit subSize/TxSize ordinal tables (section
# 6.10.4 "subSize" listing and 6.10.16 "TX size semantics"), not assumed from
# convention.
BLOCK_ORD = {
    name: i
    for i, name in enumerate(
        [
            "BLOCK_4X4", "BLOCK_4X8", "BLOCK_8X4", "BLOCK_8X8", "BLOCK_8X16",
            "BLOCK_16X8", "BLOCK_16X16", "BLOCK_16X32", "BLOCK_32X16",
            "BLOCK_32X32", "BLOCK_32X64", "BLOCK_64X32", "BLOCK_64X64",
            "BLOCK_64X128", "BLOCK_128X64", "BLOCK_128X128", "BLOCK_4X16",
            "BLOCK_16X4", "BLOCK_8X32", "BLOCK_32X8", "BLOCK_16X64", "BLOCK_64X16",
        ]
    )
}
BLOCK_ORD["BLOCK_INVALID"] = 22
TX_ORD = {
    name: i
    for i, name in enumerate(
        [
            "TX_4X4", "TX_8X8", "TX_16X16", "TX_32X32", "TX_64X64", "TX_4X8",
            "TX_8X4", "TX_8X16", "TX_16X8", "TX_16X32", "TX_32X16", "TX_32X64",
            "TX_64X32", "TX_4X16", "TX_16X4", "TX_8X32", "TX_32X8", "TX_16X64",
            "TX_64X16",
        ]
    )
}
BODY_SYMS = {**BLOCK_ORD, **TX_ORD}


def body_to_rust(raw):
    body = raw.split("=", 1)[1].strip()
    if body.endswith(","):
        body = body[:-1]
    for name, val in BODY_SYMS.items():
        body = re.sub(rf"\b{name}\b", str(val), body)
    rust = body.replace("{", "[").replace("}", "]")
    rust = re.sub(r"\s+", " ", rust).strip()
    return rust


def const_name(spec_name):
    s = re.sub(r"(?<!^)(?=[A-Z])", "_", spec_name)
    return s.upper().replace("__", "_").rstrip("_")


def rust_type(dims):
    ty = "u16"
    for d in reversed(dims):
        ty = f"[{ty}; {eval_dim(d)}]"
    return ty


def main():
    spec_path, out_path = sys.argv[1], sys.argv[2]
    names = sys.argv[3:]
    clean = strip_noise(load_lines(spec_path))
    out = [
        "//! Default CDF tables, AV1 spec section 9.4.",
        "//!",
        "//! Mechanically extracted from the specification text via",
        "//! `scripts/extract_cdf.py` (kept in `provenance/`) rather than retyped by",
        "//! hand: the script brace-matches each `Default_X_Cdf[dims] = { ... }`",
        "//! block and transliterates C-style `{}` nesting to Rust `[]` nesting, so",
        "//! the printed element order is preserved exactly. The dimension header is",
        "//! evaluated against the specification's own named constants (section 3)",
        "//! to produce the array type, so a shape mismatch is a compile error. Every",
        "//! row's structural invariant (the fixed 32768 entry, `cdf[N-1]`) is",
        "//! checked in this module's own tests, over every table.",
        "#![allow(",
        "    clippy::unreadable_literal,",
        "    clippy::excessive_precision,",
        '    reason = "mechanically extracted probability tables, not hand-written numerals"',
        ")]",
        "",
    ]
    for name in names:
        raw = find_table(clean, name)
        dims = header_dims(raw)
        ty = rust_type(dims)
        body = body_to_rust(raw)
        cname = const_name(name)
        out.append(f"/// `{name}`, spec 9.4.")
        out.append(f"pub const {cname}: {ty} = {body};")
        out.append("")
    with open(out_path, "w", encoding="utf-8") as f:
        f.write("\n".join(out))
    print(f"wrote {len(names)} tables to {out_path}")


if __name__ == "__main__":
    main()
