# `vaco-codec-dsp-sinewin`

Layer 3. Sine *and* KBD window generation for MDCT-based codecs (D-06),
built as a prerequisite of AAC-LC decode (#256, T3-03), extended to KBD by
AAC-LC reconstruction (#445, T3-03c).

## What it is

`sine_window::<N>() -> [f32; N]` (stack-allocated, `N` a compile-time
constant) and `sine_window_into(out, n)` (for a window length only known at
runtime — still one of a codec's own small closed set of sizes, never
attacker-sized), implementing:

```text
win[i] = sin( (π / N) * (i + 0.5) )      for i in 0..N
```

ISO/IEC 14496-3 subpart 4 §4.6.3's `window_shape == 0`. AAC uses `N = 2048`
for a long block's 1024-line MDCT and `N = 256` for each of an
`EIGHT_SHORT` sequence's eight 128-line MDCTs; the same formula (with its own
`N`) is Vorbis's sine window and, generically, any plain (non-KBD) MDCT
codec's TDAC window.

**`kbd_window::<N>(alpha) -> [f32; N]`, AAC's other window shape
(`window_shape == 1`), added by #445.** D-06 originally named this crate for
the sine window specifically, and `vaco-codec-aac` shipped sine-only on the
working assumption that `ffmpeg`'s AAC encoder — this workspace's only
source of real AAC fixtures — never emits `window_shape == 1`. **That
assumption was wrong**: several real fixtures genuinely set it partway
through the stream (confirmed not a parsing artefact — #444's own
677-real-frame bit-consumption invariant still held exactly after the
change that surfaced this). A crate whose whole reason to exist is "the
window shapes AAC decode needs" cannot leave one of AAC's exactly two
shapes out, so KBD lives here rather than in a new crate.

## How it works

### The correctness property tested against: Princen-Bradley, not "looks sinusoidal"

ISO/IEC 14496-3 subpart 4 §4.6.3 requires any window it accepts to satisfy

```text
win[i]^2 + win[i + N/2]^2 == 1     for every i in 0..N/2
```

— the condition that makes overlap-add's time-domain aliasing actually
cancel. A window that fails this desyncs decode silently: the output looks
plausible and is wrong, the same failure class this workspace's other codec
work keeps finding by measurement rather than by inspection.
`sine_window_satisfies_princen_bradley` checks the identity directly, and the
crate's own tests hold both of AAC's real sizes (2048, 256) to it — not just
the general algebraic reason it holds for *every* even `N` (`i` and `i + N/2`
land `π/2` apart in the `sin` argument, so this reduces to `sin²θ + cos²θ ==
1`).

### No allocation, anywhere

`sine_window::<N>` returns a fixed-size array built with `std::array::from_fn`
— entirely on the stack, no `Vec`. `sine_window_into` writes into a
caller-provided buffer and returns how many samples it actually wrote
(`out.len().min(n)`), so a wrong-sized buffer is truncated rather than
panicking. A window's length is never read from untrusted input directly (it
is selected from a codec's own closed set of window sizes by a bitstream
field), so there is no `vaco-limits::Budget` to charge this against — the
right fix is simply not to allocate at all.

### `f64` internally, `f32` at the store

`sample` computes the `sin` argument and the `sin` call itself in `f64`,
rounding to `f32` only at the final store — this workspace's usual convention
(see `vaco-codec-mpegaudio`'s synthesis window) for doing transcendental work
in wider precision than the type that is ultimately kept, so rounding error is
the store's alone rather than compounded through the trig call too.

### KBD's construction, and the square-root pitfall

Unlike the sine window's closed form, KBD is built from a running sum
(§4.6.11.3.2):

```text
kernel[n]  = I0(π·alpha·sqrt(1 - ((n - N/4)/(N/4))^2)) / I0(π·alpha)   for 0 <= n <= N/2
cumsum[i]  = Σ_{n=0}^{i} kernel[n]                                     for 0 <= i <= N/2
win[n]     = sqrt( cumsum[n] / cumsum[N/2] )                           for n <  N/2
win[n]     = win[N - 1 - n]                                            for n >= N/2  (mirror)
```

`I0`, the modified Bessel function of the first kind, is evaluated by its
own defining power series (`Σ_{k=0}^∞ ((x/2)^k / k!)^2`), which converges
well within 32 terms for the `alpha` values AAC uses (4 for `N=2048`, 6 for
`N=256` — callers state `alpha` explicitly rather than this function
guessing from `N`).

**The square root is not optional, and dropping it is the shape of bug this
workspace keeps finding: plausible, not obviously wrong.** A first
implementation of `kbd_window` used `cumsum[n] / cumsum[N/2]` directly, with
no square root. It passed symmetry, monotonicity and `[0, 1]`-boundedness —
every property that "looks like a window" — and *nearly* passed
Princen-Bradley too: at `N=2048` it was off by ~1.06e-4 at one sample and
worse elsewhere (up to ~2e-3), close enough that a loose tolerance would
have let it through. The reason a square root is required, algebraically:
the kernel's own symmetry (`kernel[k] == kernel[N/2 - k]`) gives
`cumsum[m] + cumsum[N/2 - 1 - m] == cumsum[N/2]` directly — a **sum-to-one**
identity on the un-rooted ratios. Princen-Bradley needs a **sum-of-squares**
identity instead, and squaring a value that already *is* the square root of
that ratio is exactly what turns one into the other. `kbd_window`'s own doc
comment carries this derivation; `kbd_window_satisfies_princen_bradley_at_both_aac_sizes`
is the regression test that only the correct (rooted) construction passes,
at a `1e-4` tolerance both AAC sizes clear easily once the root is in.

## How to change it

- A new codec needing a plain sine MDCT window at a size this crate has not
  been tested at: add the size to the Princen-Bradley and symmetry tests
  rather than trusting the general formula untested — cheap insurance, and
  it's exactly how the existing 2048/256 pair was chosen (AAC's own two
  sizes, not arbitrary round numbers).
- Same for a new `alpha`/`N` pair for KBD: add it to
  `kbd_window_satisfies_princen_bradley_at_both_aac_sizes` (or a new test
  alongside it) rather than trusting the construction at an untested size —
  the Bessel series' convergence and the cumulative sum's own floating-point
  error both scale with `N` and `alpha` in ways worth checking, not assuming.
- If a future codec needs a *third* window shape: keep holding it to the
  same Princen-Bradley test both shapes here already use — the property is
  shape-independent, and it is the one test that has actually caught a real
  bug in this crate (the missing square root above).

## Configuration

None. No features, no environment variables.

## Dependencies

None beyond `std`. No external runtime dependencies, no other `vaco-*` crate.

## Verification

Unit tests in `src/lib.rs`:

- **Sine**: Princen-Bradley held at both of AAC's real sizes (2048, 256),
  window symmetry (`win[i] == win[N-1-i]`, from `sin(π - x) == sin(x)`),
  every sample bounded to `[0, 1]`, an `N=4` case recomputed independently
  by hand against the formula, `sine_window_into` truncating correctly on a
  too-small buffer, and a zero-length window never panicking.
- **KBD**: Princen-Bradley held at both of AAC's real `(N, alpha)` pairs
  (2048/4, 256/6 — see "the square-root pitfall" above for why this test is
  the one that matters), symmetry, and every sample bounded to `[0, 1]` and
  monotonically increasing on the left half (a KBD window's own defining
  shape, distinct from sine's).

No fixtures needed for either shape — like `vaco-codec-vlc`, this crate's
correctness is a mathematical property checked directly, not a
format-specific behaviour compared against a reference decoder. (AAC
reconstruction's own correctness, which *does* need a reference-decoder
comparison, is `vaco-codec-aac`'s to verify — see
`docs/codec/vaco-codec-aac.md`'s "Decode accuracy".)
