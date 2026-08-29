//! `drawtext`'s `expansion=normal` directive set (plan 16 SS6.2): `%{...}`
//! substitution inside the `text` option, evaluated once per frame.
//!
//! Implemented: `%{pts[:fmt[:offset]]}` (`fmt` one of `flt`/`hms`, default
//! `flt`), `%{n}`/`%{frame_num}`, `%{metadata:key[:default]}`,
//! `%{expr:EXPR}` (via [`vaco_expr`], with `n`/`t` bound). `%%` escapes a
//! literal `%`.
//!
//! Not implemented: `%{eif:...}`, `%{gmtime}`, `%{localtime}`,
//! `%{pict_type}`, `%{expr_int_format}` — real gaps, not silently guessed;
//! an unrecognised `%{...}` directive passes through verbatim (matching the
//! reference's own behaviour for garbage input better than dropping it
//! would).

use vaco_expr::{Bindings, Expr};

#[derive(Debug)]
pub struct ExpandContext<'a> {
    pub pts_seconds: Option<f64>,
    pub frame_num: i64,
    pub metadata: &'a [(String, String)],
}

/// Expand every `%{...}` directive in `text` once.
#[must_use]
pub fn expand(text: &str, ctx: &ExpandContext<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'%') && bytes.get(i + 1) == Some(&b'%') {
            out.push('%');
            i += 2;
            continue;
        }
        if bytes.get(i) == Some(&b'%')
            && bytes.get(i + 1) == Some(&b'{')
            && let Some(end) = text[i + 2..].find('}')
        {
            let directive = &text[i + 2..i + 2 + end];
            out.push_str(&expand_one(directive, ctx));
            i += 2 + end + 1;
            continue;
        }
        let Some(c) = text[i..].chars().next() else { break };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn expand_one(directive: &str, ctx: &ExpandContext<'_>) -> String {
    let mut parts = directive.splitn(2, ':');
    let name = parts.next().unwrap_or("");
    let rest = parts.next();
    match name {
        "pts" => format_pts(rest, ctx.pts_seconds),
        "n" | "frame_num" => ctx.frame_num.to_string(),
        "metadata" => expand_metadata(rest, ctx.metadata),
        "expr" => expand_expr(rest, ctx),
        _ => format!("%{{{directive}}}"),
    }
}

fn format_pts(rest: Option<&str>, pts: Option<f64>) -> String {
    let Some(pts) = pts else { return String::new() };
    let mut sub = rest.unwrap_or("flt").splitn(2, ':');
    let fmt = sub.next().unwrap_or("flt");
    let offset: f64 = sub.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let value = pts + offset;
    match fmt {
        "hms" => {
            let neg = value < 0.0;
            let total_cs = (value.abs() * 100.0).round() as i64;
            let (h, m, s, cs) = split_centiseconds(total_cs);
            format!("{}{h:02}:{m:02}:{s:02}.{cs:02}", if neg { "-" } else { "" })
        }
        _ => format!("{value:.6}"),
    }
}

/// `total_cs` (non-negative centiseconds) as `(hours, minutes, seconds,
/// centiseconds)` — this is deliberately integer arithmetic on a duration,
/// not a lossy quantity, so `clippy::integer_division` (denied workspace-
/// wide as a guard against silent precision loss elsewhere) does not apply
/// here.
#[allow(clippy::integer_division, reason = "exact base-60/100 decomposition of a duration, not a lossy division")]
fn split_centiseconds(total_cs: i64) -> (i64, i64, i64, i64) {
    let (h, rem) = (total_cs / 360_000, total_cs % 360_000);
    let (m, rem) = (rem / 6_000, rem % 6_000);
    let (s, cs) = (rem / 100, rem % 100);
    (h, m, s, cs)
}

fn expand_metadata(rest: Option<&str>, metadata: &[(String, String)]) -> String {
    let mut sub = rest.unwrap_or("").splitn(2, ':');
    let key = sub.next().unwrap_or("");
    let default = sub.next().unwrap_or("");
    metadata.iter().find(|(k, _)| k == key).map_or_else(|| default.to_owned(), |(_, v)| v.clone())
}

fn expand_expr(rest: Option<&str>, ctx: &ExpandContext<'_>) -> String {
    let Some(src) = rest else { return String::new() };
    let bindings = Bindings::new(&["n", "t"]);
    let Ok(expr) = Expr::parse(src, &bindings) else {
        return String::new();
    };
    let t = ctx.pts_seconds.unwrap_or(0.0);
    #[allow(clippy::cast_precision_loss, reason = "frame_num display precision loss is inconsequential")]
    let vars = [ctx.frame_num as f64, t];
    format!("{}", expr.eval(&vars))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ExpandContext<'static> {
        ExpandContext { pts_seconds: Some(1.5), frame_num: 42, metadata: &[] }
    }

    #[test]
    fn literal_percent_escapes() {
        assert_eq!(expand("100%%", &ctx()), "100%");
    }

    #[test]
    fn frame_num_substitutes() {
        assert_eq!(expand("frame %{n}", &ctx()), "frame 42");
    }

    #[test]
    fn pts_default_format_is_seconds() {
        assert_eq!(expand("%{pts}", &ctx()), "1.500000");
    }

    #[test]
    fn pts_hms_format() {
        assert_eq!(expand("%{pts:hms}", &ctx()), "00:00:01.50");
    }

    #[test]
    fn metadata_falls_back_to_default() {
        assert_eq!(expand("%{metadata:title:untitled}", &ctx()), "untitled");
    }

    #[test]
    fn unrecognised_directive_passes_through() {
        assert_eq!(expand("%{gmtime}", &ctx()), "%{gmtime}");
    }

    #[test]
    fn expr_directive_evaluates() {
        assert_eq!(expand("%{expr:2*n}", &ctx()), "84");
    }
}
