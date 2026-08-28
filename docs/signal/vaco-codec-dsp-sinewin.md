# `vaco-codec-dsp-sinewin`

Layer 3. Sine window generation for MDCT-based codecs (D-06), built as a
prerequisite of AAC-LC decode (#256, T3-03).

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

**KBD (Kaiser-Bessel-Derived), AAC's other window shape, is not in this
crate.** It is a materially different, iterative construction — a running sum
over Bessel-function terms, not a closed-form `sin` — and D-06 names this
crate for the sine window specifically. `vaco-codec-aac` ships sine-only for
the same reason; KBD is a disclosed gap there, not silently approximated here.

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

## How to change it

- Adding KBD: it does not belong in this crate under its current name: either
  extend this crate with a second function family and consider renaming it,
  or give KBD its own crate, mirroring the "one shape per module" split D-01's
  own `vaco-codec-vlc` doc argues for. Either way, hold it to the same
  Princen-Bradley test this crate uses for the sine window — the property is
  shape-independent.
- A new codec needing a plain sine MDCT window at a size this crate has not
  been tested at: add the size to the Princen-Bradley and symmetry tests
  rather than trusting the general formula untested — cheap insurance, and
  it's exactly how the existing 2048/256 pair was chosen (AAC's own two
  sizes, not arbitrary round numbers).

## Configuration

None. No features, no environment variables.

## Dependencies

None beyond `std`. No external runtime dependencies, no other `vaco-*` crate.

## Verification

Unit tests in `src/lib.rs`: Princen-Bradley held at both of AAC's real sizes
(2048, 256), window symmetry (`win[i] == win[N-1-i]`, from `sin(π - x) ==
sin(x)`), every sample bounded to `[0, 1]`, an `N=4` case recomputed
independently by hand against the formula, `sine_window_into` truncating
correctly on a too-small buffer, and a zero-length window never panicking. No
fixtures needed — like `vaco-codec-vlc`, this crate's correctness is a
mathematical property checked directly, not a format-specific behaviour
compared against a reference decoder.
