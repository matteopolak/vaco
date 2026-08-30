# End-to-end gaps, measured 2026-08-29

Measured by building `vaco`/`vaco-probe` and running real invocations against
real ffmpeg-produced media. **None of these are missing codecs.** Every one is
integration glue between pieces that already exist and already work.

## What already works

- `vaco-probe` on mp4, mkv, mp3, wav — including `-print_format json -show_streams`.
- Stream-copy remux: mp4→mkv, mp4→mp4, mp4→ts, wav→wav.
- Audio filtering: `-af volume=0.5` through to a real wav.

## 1. H.264 decode is unreachable from the binary — the top blocker

`H264Decoder::send_packet` resolves the slice header's `pic_parameter_set_id`,
reads `entropy_coding_mode_flag` off the PPS, and **stops**. Its own module doc
says so. It has never decoded a pixel.

Meanwhile `reconstruct_picture_with_inter` and `reconstruct_picture_luma` decode
`cabac_ip_simple.264` **100% byte-exact against ffmpeg, all 25 frames,
0/102400 luma samples differing** — and both are `pub(crate)` with **no caller
anywhere in `src/`**. The only thing that drives them is a `#[cfg(test)]` module
inside `reconstruct.rs`.

So the entire H.264 decoder is reachable only from its own tests. `vaco -i
any.mp4` cannot decode video. This is the inverse of the "an API with no caller
is invisible to every test you will write" rule: here the *implementation* has
no production caller, and the tests hid that by driving it directly.

Needs: an access-unit driver in `H264Decoder` (reusing
`H264Parser::push_access_unit` rather than duplicating it), DPB/output ordering,
and AVCC-vs-Annex-B handling. MP4 stores length-prefixed AVCC; the decoder wants
Annex-B; `h264_mp4toannexb` exists and is registered but **nothing in the decode
path ever applies it**.

## 1b. Measured H.264 capability (corrected 2026-08-29, real binary)

**A real H.264 stream now decodes end to end.** Measured with a scripted
harness against `libx264`-encoded `testsrc2`, after the deblocking fix in
`e63c09f`:

| Input | Result |
|---|---|
| Main, `-bf 0 -refs 1` | **FULL 25/25 frames** |
| Main, `-bf 0` (refs 3) | 2/25 — CABAC desync, needs multiple references |
| Main, default (B-frames) | 2/25 — `CABAC B-slice mb_type/sub_mb_type` |
| High, any (**x264's own default**) | 0/25 — `transform_size_8x8_flag`/Intra_8x8 |
| Baseline | 0/25 — CAVLC reconstruction unimplemented |

**An earlier version of this section was wrong** and is corrected here. It
claimed Main with `-bf 0` reached only 2 frames and blamed a CABAC desync on
plain P slices. Two errors: `-bf 0` alone still leaves `-refs 3`, and the
measuring harness silently reported `0/25` for every row because a partial
write met an exact-size comparison. The desync is real but is **multi-reference
only** — single-reference Main content decodes completely.

Remaining work, ordered by how many real files each blocks:

1. **Intra_8x8 / `transform_size_8x8_flag`** — High profile is x264's default,
   so this alone excludes most files in existence.
2. **CABAC B slices** — x264 emits B-frames by default at every profile.
3. **The multi-reference CABAC desync** — x264 defaults to `-refs 3`. Sharpest
   known repro: slice 4 of `cabac_ip_multiref.264` stops at 35 of 36
   macroblocks. `ref_idx_lX` binarization has been ruled out against clause
   9.3.3.1.1.6.
4. **CAVLC reconstruction** — the entropy layer verifies bit consumption and
   discards its coefficients and motion vectors.

All four are unfinished implementation over a decoder now proven correct on a
real stream, not design problems.

## 2. Matroska rejects codecs it should map

`the muxer refused a stream: unsupported: matroska: codec has no CodecID
mapping` for `ffv1` and `flac` — both of which we implement. A table gap.

## 3. No automatic sample-format conversion for muxer constraints

`wav: planar sample formats are not supported`. Decoding AAC yields planar
float; wav needs packed. ffmpeg inserts the conversion automatically; we refuse
the stream instead. `vaco-codec-dsp-fmtconvert` and `vaco-resample` both exist.

## 4. `-f null` has no default encoder

`Default encoder for format null (codec none) is probably disabled`. `-f null -`
is one of the most common invocations there is (decode-and-discard timing runs).

## 5. mkv → mp4 stream copy fails on timestamps

`non-monotonic dts: this container requires strictly increasing timestamps`.
Matroska carries DTS the mp4 muxer then rejects.

## The pattern

Four separate defects today were "registered but not actually reachable" — the
bitstream-filter crates, two empty `FormatFlags` declarations, the Ogg/Theora
extradata gap, the `.jls` extension gap. This list is the same class one level
up: **the pieces work; the wiring between them does not.** Verification that
drives internals directly cannot see any of it. Only running the binary can.

## 6. Benchmark findings (2026-08-29, vaco vs ffmpeg)

Three scenarios the owner named, measured with the real binaries.

| Scenario | ffmpeg | vaco |
|---|---|---|
| mkv → mp4 stream copy (1080p, 5s) | 0.09s, 3,411,126 B | **0.06s**, 3,412,352 B |
| 2160p → 1080p decode+scale+rawvideo | 0.35s, 233,280,000 B | exit 0, **0 bytes** |
| H.264 → H.265 via `libx265` (1080p) | 3.53s, 2,409,789 B | `max_alloc_total limit exceeded` |

**Stream copy is genuinely faster than ffmpeg.** That is the one path measured
end to end on both sides, and vaco wins it.

### 6a. H.264 SPS parsing fails at level 5.1

Bisected by resolution, all Main profile, `-bf 0 -refs 1`, CABAC:

| Resolution | Level | `vaco-probe` |
|---|---|---|
| 320×240 | 13 | `Main`, `yuv420p` |
| 1280×720 | 31 | `Main`, `yuv420p` |
| 1920×1080 | 40 | `Main`, `yuv420p` |
| **3840×2160** | **51** | **`unknown`, `unknown`, `level=-99`** |

The whole SPS parse fails, so `pix_fmt` is `none` and the decoder produces zero
frames. `max_dimension` is not the cause — `permissive()` allows 65,536 and
`strict()` allows 8,192, both well over 3840.

**And it exits 0 having decoded nothing.** A run that produces no frames must
not report success; that is what made this look like a fast benchmark result
rather than a failure.

### 6b. `max_alloc_total` exceeded on a 1080p transcode

`vaco -i hd.mkv -an -c:v libx265 -f matroska out.mkv` on 1920×1080 hits the
1 GiB `permissive()` ceiling. A 1080p yuv420p frame is ~3.1 MB, so a handful of
DPB frames plus filter buffers should be nowhere near it. Either something
retains every frame, or the budget is being charged for allocations that are
freed.

## 7. "FULL 25/25" meant frame count, not correctness — correction

Every H.264 result reported in §1b and §6 measured **output size**, not output
content. Comparing actual decoded bytes against ffmpeg's decode of the same
file (320×240, Main, `-bf 0 -refs 1`, the configuration reported as "FULL"):

| Frame | Y bytes differing | max Δ | mean Δ | U max Δ | V max Δ |
|---|---|---|---|---|---|
| 0 (IDR) | 48 / 76800 | 3 | 0.00 | 1 | 1 |
| 1 | 528 | 47 | 0.03 | 55 | 144 |
| 2 | 2,861 | 66 | 0.28 | 142 | 144 |
| 5 | 5,459 | 125 | 0.74 | 142 | 235 |
| 12 | 13,064 | 136 | 2.73 | 158 | 238 |
| 24 | 16,022 (21%) | 133 | 2.78 | 187 | 232 |

**13.70% of all bytes differ across the sequence.**

The intra frame is essentially correct — 48 samples off by at most 3, which is
the known deblocking rounding gap. **Everything after it drifts**, monotonically,
and chroma drifts worse than luma. Each P frame predicts from the previous one,
so an error in inter prediction or reference handling compounds.

By the owner's own shipping rule this is not shippable: the deviation is
**structured** (it grows with frame index and concentrates in chroma), not the
small unstructured rounding difference that rule permits.

This is the same failure mode this document already names twice — a harness
measuring the wrong thing. §1b's `0/25` rows came from comparing an exact size
against a partial write; these `FULL 25/25` rows came from comparing a size and
never looking at a byte. **A decoder that emits the right number of bytes has
demonstrated only that it emits the right number of bytes.**

Correct acceptance for any decoder here: per-plane byte comparison against the
reference across the **whole sequence**, not the first frame and not the size.

## 8. `H264Decoder`'s DPB never releases its budget (diagnosed, not fixed)

`self.dpb.pop_front()` drops reference pictures without `budget.release(...)`,
and the reconstructed picture's own charged buffers are never released either.
`Budget`'s `committed` counter therefore only ever rises, disconnected from what
is live. Measured: 1080p `-refs 1 -bf 0` fails after exactly **10 frames** with
~65.5 MB committed against a real working set of ~9–12 MB. At 4K it trips after
2 frames.

Real memory is freed correctly; the budget simply never hears about it. **Raising
`max_alloc_total` would not fix this** — the counter is unbounded until `flush()`,
so a larger cap only moves the frame at which it fires.

## 9. Benchmark results, measured (2026-08-29, after the DPB fix)

4K now decodes completely — all 75 frames, 933,120,000 bytes — where it was
capped at 2 frames before the budget-release fix.

| Scenario | ffmpeg | vaco | Ratio |
|---|---|---|---|
| mkv → mp4 stream copy (5s clip) | 0.06s | 0.03s | too short to measure — see below |
| 2160p → 1080p decode+scale+rawvideo | 0.26s | 7.53s | 29× slower |
| H.264 → H.265 via `libx265` | 1.94s | 5.35s | 2.8× slower |

### How much of that is threading

Isolated on a 10-core machine, 4K decode only:

| | Time |
|---|---|
| ffmpeg, default threads | 0.24s |
| ffmpeg, `-threads 1` | 0.61s |
| **vaco** (no threading at all) | **6.53s** |

So threading buys ffmpeg ~2.6× here, and **vaco is ~10.7× slower than
single-threaded ffmpeg**. Two separate problems, and the larger one is not
threading:

1. **~10× per-frame work.** SIMD coverage, memory layout, and algorithmic
   choices in the decode path. This is where the real gap is.
2. **No threading.** Frame- and slice-level parallelism do not exist. `vaco-sched`
   has machinery; no decoder uses it.

Stream copy — the one path that is neither decode-bound nor encode-bound — is
already faster than ffmpeg, which suggests the I/O and container layers are
sound and the gap is specifically in codec inner loops.

### Caveats on these numbers

- **The 2160p→1080p output is not correct yet.** H.264 inter prediction drifts
  (§7), so this times wrong pixels. The figure will move once that is fixed —
  possibly in either direction.
- **The H.265 comparison is not like-for-like**: vaco's output is 7,111,408
  bytes against ffmpeg's 2,409,789, so different parameters are reaching x265.
  Until the invocations match, that row measures configuration, not speed.


### Correction: the stream-copy number was noise

The 5-second clip above is too short to time. Re-measured on a **60-second,
1080p, 44 MB** file:

| | Time |
|---|---|
| ffmpeg remux | 0.075s |
| vaco remux | **0.073s** |

**Parity, not a 2× win.** The earlier figure was measurement noise at a
duration where process startup dominates. The useful conclusion survives —
the I/O, demux and mux layers are not where the time goes — but "faster than
ffmpeg" was never a real result and should not have been reported as one.

## 10. Profiling loop, round 1 (samply)

Baseline established on a private `--target-dir`, because a concurrent agent's
HEVC build kept overwriting `target/release/vaco` mid-measurement. Timings are
best-of-8 from an **interleaved** A/B that alternates baseline and candidate
within each round; a niced fuzz sweep was running throughout, and sequential
timing could not be trusted against it.

| | best-of-8 |
|---|---|
| ffmpeg, default threads | 0.24s |
| ffmpeg, `-threads 1` | 0.61s |
| vaco, before round 1 | 6.07s |
| vaco, after round 1 | **5.49s** |

~1.11x, narrowing the single-threaded gap from 10.7x to ~9.0x. Current beat
baseline in 8 of 8 interleaved rounds.

Landed:
- `d494531` row-wise `copy_from_slice` in place of per-pixel `set_pixel` /
  `set_chroma_pixel` in the reconstruction loops (~3.5%).
- `48a03cf` move the decoded residual into `MbSummary` instead of deep-cloning
  it per macroblock; only a slice's first macroblock needs a second copy (~3%).
- `96b020a` skip six-tap edge clamping when a 4x4 block's whole reach is
  provably in bounds (~3%, noisier).

**Negative result, reverted:** batching deblocking's per-pixel reads and writes
into contiguous slice operations was mechanically sound, measured as a
wash-to-slight-loss (slower in 6 of 8 rounds), and showed no drop in
`deblock_picture_luma` self time. This is the third recorded SIMD/memory
negative result in this repo, after `add_pixels_clamped_vector` (0.9x/0.84x,
gated to scalar) and 2 of 8 earlier kernels lost to autovectorization.

**Correctness caveat that shapes this whole loop.** The H.264 decoder's output
is *wrong* — 13.70% of bytes differ from ffmpeg. So no optimisation here can be
regression-checked against ffmpeg. Each round instead captures the exact output
bytes before its first change and must reproduce them byte-for-byte. That keeps
the optimisation honest but explicitly does **not** move correctness; the drift
is tracked separately and is the more important defect.

Remaining profile after round 1: `reconstruct_inter_mb` 21.1%,
`reconstruct_picture` 19.0%, `deblock_picture_luma` 16.1%,
`deblock_picture_chroma` 8.7%, `residual_block_cabac` 6.6%. Ranks 1-4 are ~65%
of runtime and are the six-tap luma filter, the chroma filter, and deblocking
arithmetic -- all genuine SIMD shapes, which is round 2.

**Threading is not in this loop and is the larger untouched factor.** ffmpeg
gains 2.5x from its default frame threading (0.61s to 0.24s); vaco decodes
single-threaded. `vaco-sched` already has `Driver::with_threads`, but that is
pipeline-stage parallelism, which buys almost nothing on a decode-bound job.
Frame-level threading inside the decoder is a DPB-refactor-sized change and is
deliberately deferred until the inter-prediction drift is fixed -- parallelising
a decoder that is still producing wrong pixels would make the drift harder to
bisect, not easier.

## 11. Profiling loop, round 2 — three attempts, zero commits

Round 2 targeted the interpolation and deblocking kernels (~65% of runtime).
It landed nothing, which is the correct outcome: every change was measured and
none cleared the noise floor.

| attempt | result |
|---|---|
| Lazy two-axis "j" derivation in `interp::luma_qpel_sample` (6 raw six-tap sums computed eagerly on every call; only 5 of 16 fractional arms use them) | ratio 0.997, won 4/8 — wash, reverted |
| Windowed gather in `fetch_pred_4x4`: fetch the 9x9 window once instead of re-reaching per output pixel (up to 36 overlapping re-fetches at clause 8.4.2.2.1's "j" position) | ratio 1.0025, won 6/10 — wash, reverted |
| Chroma bilinear in-bounds fast path in `predict_chroma_inter`, mirroring luma's | ratio **1.034 — a 3.4% regression**, won 2/10, reverted |

The chroma regression is instructive: chroma's `.clamp()` is two cheap ops per
sample, so the guard branch cost more than the clamp it skipped. Luma's
equivalent fast path won in round 1 because luma's dual-axis clamp is genuinely
expensive. **The same transformation was a win on one plane and a loss on the
other**, which is exactly why each is measured rather than reasoned about.

All three were byte-identical in output, so this is purely a performance
verdict, not a correctness one.

**The real blocker, now named.** Vectorizing the deblocking arithmetic needs a
**masked lane select** primitive that `vaco-simd` does not have — its filter
decisions are per-sample (`bS`, `|p0-q0| < alpha`, `|p1-p0| < beta`, and the
per-sample strength tests), so a vector kernel must compute candidates and
select per lane from a mask. `vaco-codec-dsp-deblock`'s own docs already
recorded this. A separate pass is adding the primitive.

Also ruled out this round: `vaco-codec-dsp-mc`'s existing dispatched
`fir_row`/`TapSet<6>` kernel cannot be reused here — its batching wins come
from whole rows, and H.264's 4x4/8x8 block granularity is too narrow to fill
vector lanes profitably.

**Honest conclusion for the interpolation path: round 1 took the available
low-risk wins.** Further progress needs either a wider-batch kernel shape than
4x4/8x8 offers, or the deblocking masked-select route.

## 12. H.264 decode is byte-exact against ffmpeg — §7's 13.70% closed

§7's number was real and its acceptance rule was right. Re-measured with
exactly that rule — **per plane, per frame, byte for byte, whole sequence,
naming the first differing frame** — the same 25-frame 320×240 Main
`-bf 0 -refs 1` `libx264` clip now decodes **byte-exact, 25/25, all three
planes, 0 of 2 880 000 bytes differing.**

| Configuration (25 frames, 320×240, `testsrc2`) | Before | After |
|---|---|---|
| Main, `-bf 0 -refs 1` | 17.62%, from frame 0 | **0.0000%, byte-exact** |
| High, `-bf 0 -refs 1` (**x264's default profile**) | 6.51%, from frame 0 | 0.0008% (24 bytes), from frame 22 |
| Main, `-refs 3` | 2 frames, then refused | 2 frames **byte-exact**, then refused |
| Main, B frames | 2 frames, then refused | 2 frames **byte-exact**, then refused |
| Baseline | refused | refused |

(The 17.62% differs from §7's 13.70% only because the clip was regenerated;
same encoder settings, same shape of error, same first-frame signature —
48–60 luma samples at max delta 3.)

Four defects, in the order the sequencing rule found them — frame 0 taken to
byte-exact before any later frame was examined:

1. `vaco-codec-dsp-deblock`'s `TC0_TABLE`, **23 of 52 rows wrong** (off by
   one row for every `indexA ≥ 16`). All of frame 0's error. `2fb318e`.
2. Clause 8.4.1: an **intra neighbour was treated as an absent one**,
   conflating clause 6.4 macroblock availability with clause 8.4.1.3.2's
   `refIdxLXN = -1` substitution. Every `P_Skip` next to an intra neighbour
   mispredicted `(0, 0)` and fed the error forward. Alone took the sequence
   from 17.29% to byte-exact. `09d3078`.
3. `Intra_8x8` missing from the deblocking filter's own intra test, plus two
   more 8x8-transform gaps (`bS = 2` read the wrong residual field; the
   internal edges at offsets 4 and 12 were filtered). High profile 6.51% →
   0.0008%. `775f3d9`.

**Still open, and unchanged**: CAVLC reconstruction, CABAC B slices, the
multi-reference CABAC desync, MBAFF/field pictures. All are *refused*, not
mis-decoded. Plus one new, precisely-located residual: High profile, frame
22, macroblock (15, 13) — an `Intra_8x8` macroblock inside a **P** slice,
top row only, 24 bytes across frames 22–24. Reproduces with the loop filter
disabled on both sides, so it is reconstruction, not deblocking.

**Confirmed systematic, not a one-off**, by widening the corpus after the
fixes landed. Main `-bf 0 -refs 1` is byte-exact on four clips at four
resolutions and four content types (64×64 `cabac_ip_simple`, 176×144
`smptehdbars`, 320×240 `testsrc2`, 352×288 `testsrc`, 640×480
`mandelbrot`). High profile is byte-exact on 176×144 `smptehdbars` and
leaves 0.0049% on 352×288 `testsrc` (first differing frame 12) — and that
second repro has the **same signature**: macroblock (4, 15), `Intra_8x8`
inside a P slice. So the remaining defect is specifically `Intra_8x8`
prediction when the macroblock's neighbours are inter, and it needs a
corpus of exactly that shape to chase.

### What this says about the harness rule, again

§7's correction ("a decoder that emits the right number of bytes has
demonstrated only that it emits the right number of bytes") is now joined by
a second, sharper one. `vaco-codec-dsp-deblock`'s `TC0_TABLE` had **already
been corrected once against the oracle** — one entry changed because it took
a fixture's whole-picture match from 98.97% to 99.78%. That entry's correct
value was wrong in its *other two* columns, and the "fix" made the table
less correct while making the number go up. The hand-trace that followed
checked every input to the filter *except* that table, precisely because it
read as settled.

**Fitting one value to an aggregate difference percentage is not a check on
that value.** A near-miss percentage is evidence that the wrong entries are
rarely reached, not that they are right. The table was settled in ten
minutes by extracting JM's own and diffing all 52 rows mechanically.

### Method

Every one of the four fell to the same technique, and none to reading
specification prose: clone and build **JM 19.1 `ldecod`**, confirm it is
byte-exact against ffmpeg on the exact clip (it was, on every clip used),
then dump the same intermediate state from both and diff it. Macroblock
types and per-4×4 motion vectors out of JM's `exit_picture` found defect 2;
dumping derived `bS` on an **IDR** picture — where no value below 3 is
reachable, so a 2 is self-evidently wrong — found defect 3 in one look.

`crates/codec/vaco-codec-h264/tests/decoder_output_matches_ffmpeg.rs` (new)
is the standing check: it drives the **registered** `H264Decoder` through its
public `set_extradata`/`send_packet`/`receive_frame` surface over all 25
frames and reports the first differing frame with per-frame per-plane
magnitudes on failure. Its own detection was self-tested by injecting a
single wrong byte per frame — §1's "the implementation had no production
caller and the tests hid that" applies to correctness harnesses too.

## 13. Round two — the fixture corpus was the bug, twice more

§12 reported Main byte-exact and High at 0.0008%, measured over `testsrc2`,
`smptehdbars` and `mandelbrot`. A wider corpus found **two more real
defects**, both invisible to those fixtures, and both now fixed. High profile
on a 640x360 `mandelbrot` clip was **7.66% wrong from frame 0** — nine times
worse than §12's own worst High-profile number — and a 416x240 `life` clip
was 10.58% wrong.

| Defect | Signature | Result |
|---|---|---|
| `Intra_8x8` `Vertical_Right`/`Horizontal_Down` dropped the `- 2*x` / `- 2*y` term in their `zVR/zHD < -1` branch (clause 8.3.2.2.5/6) | luma-only, intra, High-profile-only, wrong from the IDR, max delta 48 | 7.66% → **byte-exact** (`ba622b5`) |
| Weighted prediction (clause 8.4.2.3) not implemented — `pred_weight_table()` parsed and ignored | every inter macroblock with luma residual wrong from the first P picture; `weighted_pred_flag` is **x264's default** | 10.58% → **byte-exact** (`26ca4ad`) |

Re-verified after both: Main **and** High, `-bf 0 -refs 1`, 25 frames each,
per plane per frame byte for byte, over `life`, `mandelbrot`, `zoneplate`,
`cellauto`, `sierpinski`, `testsrc`, `testsrc2`, `smptehdbars`, `rgbtestsrc`
and `gradients`, at 176x144, 352x288, 416x240, 640x360, 640x480, 720x576,
1280x720 and 322x242 (not a multiple of 16 — cropping exercised). **Every
combination byte-exact.**

### The lesson, which is about fixtures and not about H.264

Both defects were **content-dependent**, and the corpus decided whether they
were visible:

- Flat, synthetic sources (`smptebars`, `testsrc2`) select two or three of
  the nine `Intra_8x8` prediction modes. `mandelbrot`, `life` and
  `zoneplate` select all nine. The two broken modes failed on 74% and 55%
  of the blocks that used them — and were reached almost never on flat
  content.
- An encoder only emits **non-neutral prediction weights** when the content
  has global brightness change. Every other fixture made x264 choose the
  neutral weight, under which clause 8.4.2.3.2 collapses to a plain copy and
  ignoring the whole feature is indistinguishable from implementing it.

This is a second instance of the shape §7 and §12 already record, one level
up: §7 was *a harness that measured the wrong thing*, §12 was *a table fitted
to an aggregate percentage*, and this is **a corpus that cannot express the
defect**. A byte-exact result is a statement about the fixtures, not about
the decoder. When adding a decoder fixture, pick directional, high-detail
content and content with global brightness change, and check frame 0
separately — all three defects here were already wrong on the first frame or
the first inter frame.

### And a note on oracles

Two JM instrumentation attempts during this work read buffers JM had not
filled at the dump point, and one of them "proved" a prediction mismatch
that did not exist. Both were caught by running the same comparison on a
clip already known byte-exact, and by hand-checking a single `mv = (0,0)`
block — a case whose answer is knowable without any oracle at all. **An
instrumented oracle needs its own control run before its output counts as
evidence.**

### Coverage

`fuzz/fuzz_targets/h264_decode.rs` (new, `677b569`) drives
`H264Decoder::send_packet`/`receive_frame` directly. `h264_entropy` only ever
reached the two residual-block functions — the macroblock layer,
reconstruction, intra prediction, motion compensation, weighted prediction,
the DPB and deblocking had no direct fuzz coverage at all. 1155018 runs
clean.

## 14. Profiling loop, scaling — the 2160p→1080p gap, isolated and partly closed

§9's 29x figure for `2160p -> 1080p decode+scale+rawvideo` was never split
into its two costs. Decode has had two profiling rounds (§10, §11); this one
is the scaler's, and it had never been profiled at all.

**Isolated first.** Decode-only (`-c:v rawvideo -f null -`) against
decode+scale (`-vf scale=1920:1080 -c:v rawvideo -f rawvideo`), same 4K
75-frame clip, 8 interleaved launches, wall clock: best-of-8 decode-only
7.96s, decode+scale 9.88s, per-round diff (the scaler's own share) **1.30s to
1.97s**, mean 1.71s. So on this clip the scaler is a minority of §9's 7.53s
figure — most of it is still decode, which two other rounds are already
working. `cargo bench -p vaco-scale`'s new `yuv420p_2160p_to_1080p_bicubic`
entry (exact e2e resolutions/format) reproduces this in-process at 22.6
ms/frame x 75 = 1.70s, consistent.

**Filter used**: default `scaler=auto` resolves to bicubic on both `vaco` and
`ffmpeg` (its `swscale` default too), so this is a like-for-like comparison —
no quality-for-speed swap involved.

**Profiling** used samply 0.13.1 (`--unstable-presymbolicate`) against the
release bench binary. Two snags, both worth recording for the next agent:
running the divan bench binary directly without its own `--bench` flag
silently produces zero samples (cargo passes that flag; a bare invocation
does not), and presymbolication on this binary resolved function names but no
line numbers, which combined with `#[inline]` on the callee collapsed nearly
every sample onto one call-site line in the caller and hid the real hot line.
`dsymutil` on the same binary plus `llvm-symbolizer --obj=<dSYM> --inlines`
recovers the true innermost frame per address and fixed this — full method in
`docs/signal/vaco-scale.md` §8.

**Finding, matching this document's own recurring pattern (§10, §11): the
cost was bookkeeping, not arithmetic or a missing vector instruction.**
`vaco_scale::exec::filter_h`'s horizontal tap loop runs `bank.taps` times per
output pixel — 8 taps for this exact 2x bicubic downscale — but `taps` is a
runtime field, so the loop's trip count is invisible to the optimiser.
Profiling attributed roughly half of total self time to `Iterator`/`Option`
bounds-check scaffolding around that loop (`.get()`-based window fetches,
`Zip::next`, a per-pixel `checked_mul` recomputing `d * taps`), not to the
multiply-accumulate itself.

**Fix**: specialise `filter_h` for tap counts `{2, 4, 6, 8}` (bilinear, an
unscaled cubic, an unscaled lanczos, and this downscale) by converting the
coefficient/window slices to `&[i32; N]` before the loop, so the trip count
reaches the optimiser through the type instead of a runtime read. This is not
a SIMD kernel — no `vaco-simd` dependency, no lanes — it is the same class of
fix §10 already named ("row copies replacing per-pixel writes... not
vectorisation"), applied to a filter loop instead of a reconstruction loop.
Full numbers, the differential test, and the byte-exactness check are in
`docs/signal/vaco-scale.md` §8 and `docs/core/simd-adoption-measurements.md`
Group 9. Headline: **10/10 interleaved rounds favoured the change on the
exact e2e scenario, mean ratio ≈0.80 (≈1.25x)**, up to 0.58x on other real
conversions whose tap count lands on a specialised width, and no output byte
changed (MD5-verified on a decoder-free synthetic-frame harness, so the
concurrent H.264 rework elsewhere in the tree could not contaminate the
comparison).

**Left for a future round**: `filter_v`'s equivalent loop (~10% of self time,
smaller than `filter_h`'s ~50%+, and shaped differently — its cost is in the
per-row width loop rather than the tap loop, so the same fix would not
directly apply); and the decode-side majority of §9's figure, which remains
the larger open cost.

## 15. Benchmark re-measurement, and why cross-session numbers do not compare

Measured on the 75-frame 3840x2160 Main fixture, **while a fuzz sweep and three
agents were running**, so these are loaded-machine numbers:

| | best of 3 |
|---|---|
| vaco decode | 8.11s |
| `ffmpeg -threads 1` | 0.62s |
| ffmpeg default threads | 0.17s |
| vaco decode + scale to 1080p | 9.41s |
| ffmpeg decode + scale to 1080p | 0.21s |

That is ~13x off single-threaded ffmpeg and ~48x off default-threaded ffmpeg.

**Do not read 5.49s (§10) against 8.11s here as a regression.** The two were
taken under different machine loads, and §11 measured wall-clock noise under a
background sweep at roughly 300%. Only *interleaved, same-session* A/B
comparisons are valid in this repo. Whether the correctness work — weighted
prediction, correct deblocking thresholds, `Intra_8x8` — made decode
legitimately slower by doing work it previously skipped is **plausible and
unmeasured**; it needs an interleaved A/B across those commits on a quiet
machine, not a comparison of two numbers taken hours apart.

The scaler's own share is separately measured at **1.30-1.97s** of the total
(§14), so decode dominates this scenario and remains the target.

### Harness failures, recorded because they keep recurring

Getting the five numbers above took four broken attempts, all mine:

- `ffmpeg` writing to `/dev/null` **without `-y`** exits instantly on the
  overwrite prompt, and `-v error` hides the prompt. It reported 0.03s for
  decoding 75 4K frames — physically impossible, and the only reason it was
  caught.
- `2>/dev/null` on the timed command **also swallows `/usr/bin/time`'s own
  output**, yielding empty timings.
- A shell function juggling fd 3 to separate the two stderr streams produced
  0.00s for everything.
- Building with only `patent-encumbered-hevc-decode` **overwrote the binary
  built with the h264 feature** in the same target dir, so the "decode"
  being timed was an immediate "no decoder for the input codec" exit. This
  is the same shared-target-dir trap a profiling agent hit in §10, hit again
  by the person who wrote up §10.

The fix that worked was to stop writing inline shell and use a Python script
that checks each subprocess's exit status. **A timing harness that does not
assert the command succeeded is measuring startup cost.** Sanity-check every
benchmark number against physical plausibility before believing it.
