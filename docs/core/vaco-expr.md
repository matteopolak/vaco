# `vaco-expr`

Layer 0. The arithmetic expression language that filter arguments, rate control,
`-force_key_frames` and timeline `enable=` are written in.

## What it is

Very little of the reference tool's surface takes a plain number.
`-vf scale=w='if(gt(a,16/9),1280,-1)'`, `volume=volume='-6dB'`,
`drawtext=x='(w-tw)/2'`, `-force_key_frames expr:'gte(t,n_forced*5)'` and every
timeline `enable=` take an *expression*. This crate parses that language and
evaluates it.

The language is an **interface**, not an implementation detail: user command
lines depend on it, so the grammar, the function names and the numeric results
all have to match the reference exactly. It was derived from `man ffmpeg-utils`
(§ EXPRESSION EVALUATION) and then confirmed function by function against the
shipped binary, per D7/D15 — the reference's source was never consulted.

```rust
use vaco_expr::{Bindings, Expr};

let expr = Expr::parse("if(gt(a,16/9),1280,-1)", &Bindings::new(&["a"]))?;
assert_eq!(expr.eval(&[1.85]), 1280.0);
assert_eq!(expr.eval(&[4.0 / 3.0]), -1.0);
```

## How it works

### The shape of the API, and why

Parsing allocates; evaluating does not. That split is the whole design, because
a filter parses its expression once at graph setup and then evaluates it once
per frame — or, for `aeval`, once per *sample*.

| Type | Lifetime | Contents |
|---|---|---|
| `Bindings` | parse time | the variable names, and any caller functions |
| `Expr` | parsed once, shared | a flat `Vec<Op>` arena; `Send + Sync`, cheap to clone |
| `Registers` | owned by the caller | the ten `ld`/`st` slots |
| `Context` | one per evaluation | borrowed variable values, registers, print sink, clock |

Variable names are resolved to slice indices **at parse time**, so evaluation
never compares a string. Children in the arena are `u32` indices rather than
`Box` pointers, so the whole expression is one allocation and evaluating it
walks contiguous memory.

Registers belong to the caller because the reference keeps them **between
evaluations of the same expression**: an `aevalsrc` whose body is
`st(0,ld(0)+1)` yields 1, 2, 3, 4 over successive samples. Putting them in
`Context` rather than in `Expr` also keeps `Expr` immutable and shareable across
threads.

```rust
use vaco_expr::{Bindings, Context, Expr, Registers};

let expr = Expr::parse("st(0,t*2);min(max(ld(0),0),h)", &Bindings::new(&["t", "h"]))?;
let mut regs = Registers::new();                       // survives the loop
for n in 0..3u32 {
    let vars = [f64::from(n) / 25.0, 1080.0];
    let _ = expr.eval_with(&mut Context::new(&vars, &mut regs));
}
```

### What the split costs and buys

Measured with `cargo bench -p vaco-expr` on an Apple M-series, `bench` profile,
min-of-100:

| Expression | Parse | Evaluate |
|---|---|---|
| `1280` | 78 ns | 5.0 ns |
| `if(gt(a,16/9),1280,-1)` | 505 ns | 17 ns |
| `(w-tw)/2` | 212 ns | 21.7 ns |
| `sin(t*PI*2)*h/4+h/2` | 526 ns | 36.5 ns |
| `clip(lerp(w,h,mod(t,1))+between(t,0,10)*gcd(w,h),0,4096)` | 1.12 µs | 70 ns |

A realistic per-frame loop — one compiled expression, 10 000 evaluations with
changing variables and a shared register file — runs at **24 ns per evaluation**,
41 M/s. Parsing costs roughly 25 evaluations, so the split pays for itself
within the first second of any real graph and is worth several orders of
magnitude over re-parsing per frame.

### Module map

| File | Contents |
|---|---|
| `lex.rs` | whitespace removal, the number grammar (SI prefixes, `B`, `dB`, hex, `inf`/`nan`), and `strmatch` |
| `func.rs` | the 51 builtin names with their minimum and maximum arities |
| `parse.rs` | recursive descent into the arena; every grammar quirk lives here |
| `eval.rs` | the evaluator, `Registers`, `Context`, and the numeric semantics |
| `error.rs` | the four rejection categories the reference distinguishes, plus depth |

### The grammar

```text
expr     := subexpr (';' subexpr)*            left-associative
subexpr  := term (('+'|'-') term)*            operator NOT consumed by the loop
term     := factor (('*'|'/') factor)*        left-associative
factor   := unary ('^' unary)*                left-associative, base sign last
unary    := ['+'|'-'] primary                 at most ONE sign character
primary  := number | '(' expr ')' | name | name '(' expr [, expr [, expr]] ')'
```

### Reference behaviour reproduced deliberately (D17)

Ten places where the language is not what a reader would assume. Each is marked
with a `D17:` comment at the code that implements it, and each is covered by a
captured vector in `tests/reference.rs`.

| Expression | Conventional | Reference, and us |
|---|---|---|
| `2^3^2` | 512 — `^` is right-associative everywhere else | **64** |
| `-2^2` | 4 | **-4** — the base's sign is applied after the whole `^` chain, while `2^-2` is 0.25 because the exponent takes its own sign |
| `0-20dB` | -10 | **0.1** — the additive loop does not consume the `-`; it belongs to the decibel literal |
| `"1 2"` | two tokens | **12** — whitespace is *deleted* from the string before parsing, not skipped between tokens; `"m a x ( 1 , 2 )"` is `max(1,2)` |
| `---1` | -1 | **parse error** — the unary parser takes one sign character and the number lexer takes the next, so `--1` is fine but `--PI` and `--abs(1)` are not |
| `max(1,0/0)` | 1 (`fmax`) | **NaN**, while `max(0/0,1)` is 1 — a comparison select, not `fmax` |
| `if(0/0,7)` | 0, or an error | **7** — truthiness is `x != 0`, so NaN is true; `ifnot` and `not` use `x == 0`, so NaN is false for those |
| `mod(-5,3)` | -2 (`fmod`) | **1** — floored, computed as `x - floor(x/y)*y`, which also makes `mod(5,0)` NaN |
| `ld(100)` | out of range | **register 9** — indices truncate then clamp to 0..=9; NaN becomes 0 |
| `abs.(1)` | error | **1** — names match by prefix and terminate on any byte outside `[A-Za-z0-9_]`, so `abs.` is `abs` but `abs_` is unknown. Same rule makes `PI(1)` a *trailing garbage* error rather than an unknown function |

Numeric details that are equally load-bearing and equally non-obvious:

- **Decibels are `exp2(log2(10) * x / 20)`**, not `pow(10, x/20)`. `20dB` comes
  back as `9.999999999999998`; `pow(10, 1.0)` is exactly `10.0`. The
  parenthesisation matters too — `(log2(10) * x) / 20` disagrees in the last ULP.
- **Hexadecimal is integer-only**, accumulated the way `strtoull` does, saturating
  at `u64::MAX`. `0x1p4` is *not* a hex float: it scans as `0x1`, then the SI
  prefix `p` (pico), leaving `4` as trailing garbage.
- **`bitand`/`bitor` return NaN if either input is NaN** but let the infinities
  convert, which lands on `INT64_MAX`/`INT64_MIN`. Rust's `as i64` is defined as
  saturating, so it reproduces that without relying on C's undefined behaviour.
- **`gcd` with a zero operand returns the other operand, sign included**:
  `gcd(-7,0)` is -7.
- **The depth limits are the reference's acceptance boundary**, measured by
  bisection: 99 nested parens or calls parse and 100 do not; 100 flat `+`, `*`
  or `;` operators parse and 101 do not. The first is a parse-recursion limit,
  the second a node-tree-depth limit — and the second also bounds the
  evaluator's own recursion, which is why it exists at all.

### Where we knowingly differ

Three, all deliberate, none silent.

1. **`while` gets an iteration budget.** `while(1,x)` makes the reference loop
   forever — verified: it has to be `SIGKILL`ed, since it never returns to the
   signal handler. D6 makes non-termination a fuzzing finding, so we stop at
   `Limits::max_iterations` (default 2^24) and return the last value. The budget
   is shared with `root` and `taylor` because those are individually bounded at
   1000 iterations but *nest*: three nested `taylor` calls are a billion body
   evaluations. Set the field to `u64::MAX` to get the reference's behaviour.
2. **Long flat chains are accepted.** The reference rejects `1+1+…` past 100
   operators with `ENOMEM`; our limit is the tree depth, which is the property
   that actually matters for stack safety. We are the more permissive of the two,
   which per D17's converse case breaks nothing: every command line that works
   against the reference works against us.
3. **`random`/`randomi` do not reproduce the reference's bit stream.** See below.

### The `random` divergence, with the evidence

The documented contract is reproduced — the seed is a 64-bit unsigned integer in
the addressed register, the result is in 0..1, and the state advances — but not
the exact sequence. What was established:

- The state stored *is* the returned value times 2^64: `st(0,42);random(0)` gives
  `0.5200791385896834` and a following `ld(0)` gives `9.59376676763921e18`, whose
  bits are the same mantissa with the exponent 64 higher. So the generator's
  transition function and its output are the same 64-bit quantity, truncated to
  53 bits by the round trip through the register.
- That transition is **not affine**, so it is not an LCG of any constants:
  with `F(0) = 0x3acfa029e3cc6000` and `F(1) = 0x3f7fcc2e95d8fc00`, an affine map
  predicts `F(2) = 0x442ff8333e5b9800`; the measured value is
  `0x0e0684cf688bca00`. `F(42)` misses by even more.
- Neither splitmix64, murmur3 `fmix64`, xorshift64\*, an LCG followed by a
  right-xorshift over all 63 shift amounts, nor `av_lfg`'s MD5 seeding
  reproduces `F(0)`.

Measured vectors, for whoever closes this. Each is `st(0,<seed>);random(0)`,
given as the returned f64's bits:

| seed | result bits |
|---|---|
| 0 | `0x3fcd67d014f1e630` |
| 1 | `0x3fcfbfe6174aec7e` |
| 2 | `0x3fac0d099ed11794` |
| 3 | `0x3fe74004e627f360` |
| 4 | `0x3fca82b26d181f4c` |
| 8 | `0x3fb6c216a179c79d` |
| 16 | `0x3fe404cccfe7800b` |
| 32 | `0x3fe82babb3a57c69` |
| 42 | `0x8523e80b93152800` ÷ 2^64 → `0x3fe0a47d017262a5` |

Chained from seed 42, the next two are `0x3fdbbb4b965f7109` and
`0x3fda68774155a55d`. `randomi(idx,min,max)` is `min + (max-min)*random` to
within its own last ULP, and rounds slightly differently again.

`root` has one known divergence of the same kind: a plain secant from
`(0, max)` reproduces nine of ten probed cases bit-for-bit, including
`root(ld(0)-2,1)` = 2 (outside the interval), `root(1,10)` = 10 and
`root(ld(0),10)` = 0 — but `root(cos(ld(0)),10)` converges on 5·PI/2 in the
reference where an unconstrained secant escapes. The reference evidently
constrains the iterate once the endpoints bracket a sign change; the exact rule
could not be established by black-box probing. It is listed in
`tests/reference.rs::KNOWN_DIVERGENCES` rather than guessed at.

## The probe harness

Two independent harnesses, and the second exists because the first is the shape
that plan 13 §1b warns about.

### Harness A — exact bits, through a filtergraph

```sh
ffmpeg -hide_banner -loglevel error -f lavfi \
       -i "aevalsrc=exprs='<expr>':s=1:n=1:d=1" -f f64le -
```

`aevalsrc` writes the expression's value straight into a `dbl` sample, so
`-f f64le` returns the raw 64 bits with nothing rounded or formatted in between.
That is what makes the captured vectors bit-exact. A non-zero exit is a
rejection, and the message on stderr names which of the four rejection
categories fired.

**Its weakness**: the expression passes through graph splitting, option
splitting and unescaping before the evaluator sees it. Anything concluded from
it about whitespace, quoting, commas, colons or backslashes is suspect.
Characterised rather than assumed: `movie=filename=/nonexistent/a b.txt` reports
`'/nonexistent/a b.txt'`, quoted or not, and the same with a tab — so the graph
layer passes inner whitespace through to a string option verbatim.

### Harness B — filtergraph-free, for anything whitespace touches

`-force_key_frames expr:<expr>` goes from `argv` straight to the expression
parser: no graph splitting, no option unescaping, nothing between the shell and
the evaluator. The result is read back as which packets carry the keyframe flag.

```sh
ffmpeg -hide_banner -loglevel error -f lavfi -i "color=c=black:s=64x64:r=25:d=1" \
       -c:v mpeg4 -g 250 -force_key_frames "expr:eq(n,<value expr>)" -y kf.mp4
ffprobe -hide_banner -loglevel error -show_packets -of csv=p=0 \
        -show_entries packet=flags kf.mp4 | awk '{ if ($0 ~ /^K/) print NR-1 }'
```

Every D17 item above was re-confirmed here, which is what makes them safe to
build on:

| Probe | Keyframe at | Confirms |
|---|---|---|
| `eq(n,1 2)` | 12 | whitespace deleted inside a number, by the evaluator |
| `eq(n,e q ( n , 3 ))` | 3 | whitespace deleted inside an identifier and around punctuation |
| `eq(n,round(100*(0-20dB)))` | 10 | `0-20dB` is 0.1, not -10 |
| `eq(n,2^3^2-50)` | 14 | `2^3^2` is 64, not 512 |
| `eq(n,-2^2+22)` | 18 | `-2^2` is -4 |
| `eq(n,--1+4)` / `eq(n,---1+4)` | 5 / rejected | one sign character only |
| `eq(n,ifnot(isnan(max(1,0/0)),99,7))` | 7 | `max` propagates NaN from the right |
| `eq(n,mod(-5,3)+8)` | 9 | floored modulo |
| `eq(n,st(100,6);ld(100))` | 6 | register indices clamp |
| `eq(n,abs.(5))` / `eq(n,abs_(5))` | 5 / rejected | prefix name matching |
| `eq(n,if(0/0,11))` | 11 | NaN is truthy |

The captured corpus lives in `crates/core/vaco-expr/tests/reference.rs`: 438
expressions with the reference's exact output bits, 74 of them rejections.
Regenerate it by re-running harness A over the expression column and rewriting
the table; the test asserts the corpus has not shrunk, so an empty regeneration
fails loudly rather than passing vacuously.

## How to change it

- **Adding a function.** One row in `func::BUILTINS` (name, variant, minimum and
  maximum arity) and one arm in `Expr::call`. If it needs an argument left
  unevaluated, add it to the lazy `match` at the top of `call` instead — that is
  where `if`, `while`, `taylor`, `root` and `st` live.
- **Do not "fix" a `D17:` comment.** Each names the conventional behaviour, the
  reference's behaviour, and the probe. Changing one diverges our command line
  from the reference, which is what D6 exists to prevent. Re-probe first.
- **The name-matching rule is shared** between constants, variables and function
  names: `lex::strmatch`. Changing it changes all three at once.
- **Number grammar changes belong in `lex::scan_number`**, whose suffix order is
  load-bearing: `dB` is tested before the SI prefixes because `d` is itself a
  prefix, and `B` is tested last because `2Bk` and `2kBB` are both rejected.
- **New behaviour needs a vector.** Add the expression to the corpus, capture the
  reference's bits with harness A, and re-confirm anything whitespace-adjacent
  with harness B.

## Configuration

`Limits`, passed to `Expr::parse_with` and settable per evaluation through
`Context::with_limits`:

| Field | Default | Meaning |
|---|---|---|
| `max_parse_depth` | 100 | nesting of parenthesised and argument sub-expressions |
| `max_node_depth` | 101 | depth of the resulting tree; also bounds evaluator recursion |
| `max_iterations` | 2^24 | total `while` + `root` + `taylor` iterations per evaluation |

The first two reproduce the reference's acceptance boundary and should not be
raised casually: `max_node_depth` is what keeps evaluation off the stack limit.

`Context` also carries three optional hooks, all off by default:
`with_print` (a `FnMut(value, level)` for `print`), `with_time` (pins what
`time(0)` returns, for reproducible renders and tests) and `with_functions` (the
dispatcher for names declared through `Bindings::with_functions`).

## Dependencies

- `vaco-core` — only for `From<ParseError> for vaco_core::Error`.
- `thiserror` — declared by the manifest; the error types are hand-written since
  they carry a quoted tail.
- Dev only: `proptest` (round-trip and totality properties), `divan`
  (`benches/eval.rs`, evaluation throughput on the per-frame path).

No external crate provides the language, and none could: it is an interface
defined by another program's behaviour.

## Fuzzing

Two targets, both required (D6) since this parses text taken straight off a
command line or out of a playlist.

| Target | Input | Extra property beyond "does not panic" |
|---|---|---|
| `expr_parse` | arbitrary UTF-8 | evaluating twice with the same registers is also safe |
| `expr_grammar` | a token grammar, via `arbitrary` | evaluation is deterministic, and inserting whitespace never changes the result |

```sh
cargo +nightly fuzz run expr_parse   --features expr -- -max_total_time=60
cargo +nightly fuzz run expr_grammar --features expr -- -max_total_time=60
```
