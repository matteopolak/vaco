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

## 16. Fuzz sweep: 224 targets, 5 defects, ~2.3 billion executions

The sweep ran every registered target for 75s at nice 10 while other work
continued. **219 clean, 5 findings, 0 build failures, 0 run issues.** All five
are fixed.

| target | defect |
|---|---|
| `io_buffered_reader` | `MemorySource::seek` clamped past-EOF against its own comment; `IoContext::skip` discarded seek's landing position |
| `registry_discovery` | JPEG SOF precision unvalidated, printing `bits_per_raw_sample=164` into probe output |
| `mxf_demux` | `IndexDuration` of 144 quadrillion on a 9,934-byte file drove a 16.7M-iteration loop twice |
| `parse_vpx` | **false harness invariant** — VP9 §7.2.3's `frame_width_minus_1 + 1` legally reaches 65536 |
| `webp_decode` | VP8L encode inferred alpha from pixel *content*, dropping the plane on all-opaque `Rgba` |

Four of five were genuine library defects; the single harness fix was accepted
only against a cited specification clause, and was corroborated by the crate's
own `MAX_DIMENSION` already treating the bound as inclusive.

### What the sweep was actually worth

Two of these are user-visible in ordinary use, not merely fuzz artifacts:

- **JPEG precision** fabricated metadata in `vaco-probe` output from any
  corrupted file. Verified by patching one byte of a real JPEG: `164` before,
  `N/A` after — which is what `ffprobe` prints for the same file.
- **MXF** `open()` on the artifact dropped from ~500ms to ~107us, about 4,900x.
  That is a denial-of-service shape on a malformed file, and the `Limits`
  machinery exists precisely to bound it — the fix reused the essence
  container's already-computed real size rather than inventing a constant.

### The generalisation that mattered more than any single fix

`registry_discovery`'s assertion named a **class**. Auditing its sibling
assertions found four more unvalidated syntax elements reaching output that
the corpus had never reached: mp4's `nal_length_size` fabricated from a
reserved encoding, ALAC's unchecked `bitDepth`, `vpcC`'s depth of 0, and MXF's
`ComponentDepth` admitting 0 and 65..=255.

`webp_decode`'s audit was the mirror image and equally useful: PNG, QOI, FFV1,
GIF, TIFF, EXR, TGA, BMP, SGI, PCX, XWD, XBM and the PAM family were all
checked for the same content-versus-declared-format inference and were **all
clean** — every one dispatches on `match format`. A negative audit is a result;
it stops the next agent redoing the work.

## 17. Verified capability, measured by the orchestrator

Every row below was measured by decoding with the **real `vaco` CLI** and
byte-comparing against plain `ffmpeg`, per plane and per frame, on content and
frame sizes the implementing agents did not use. Not agent-reported.

| input | result |
|---|---|
| **fully stock `libx264`** (B-frames, b-pyramid, 3 refs, CABAC, weighted P), 322x242 / 640x360 / 1024x576 / 1280x720 | byte-exact, 50 frames each |
| H.264 4K `uhd.mp4`, 75 frames | byte-exact (sha256 match) |
| H.264 **60s 1080p `big.mkv`, 1800 frames** | byte-exact (sha256 match) |
| **fully stock `libx265`** (no `-x265-params` at all), 416x240 / 352x288 / 322x242 | byte-exact, 50 frames each |
| stock `libx265` 1920x1080 and **3840x2160** | byte-exact, 25 frames |
| HEVC all-intra 300x500 (partial CTU row *and* column) | byte-exact, 25 frames |

Both codecs decode ordinary encoder output byte-exactly, at real resolutions,
through the shipping CLI.

Still refused, honestly rather than decoded wrongly: H.264 CAVLC
reconstruction, temporal direct, long-term references, MBAFF/field, >1 slice
per picture, 4:2:2/4:4:4; HEVC long-term references, dependent/multi-segment
slices, tiles, I_PCM, custom scaling lists, non-4:2:0, non-8-bit, range and
screen-content extensions.

### What the day's method actually was

Nearly every defect found was an **instance of a class**, and the class always
had more members than the corpus or fixture happened to reach:

- One fuzz assertion caught JPEG printing `bits_per_raw_sample=164`; auditing
  its sibling assertions found four more unvalidated fields reaching output.
- One rejected 4K HEVC file exposed `check_frame(w, h, 4)` charging RGBA bytes
  for 4:2:0; the same overcharge was then found in ProRes, VC-1, Theora, TIFF
  and the generic container check — the *same defect already fixed in H.264*
  earlier the same day.
- One WebP round-trip failure prompted an audit of thirteen image encoders,
  which came back **clean** — a negative result, and worth the same record.

The second recurring shape: **a fixture that cannot express a bug proves
nothing.** Neutral weights hid weighted prediction in both codecs. Flat content
hid `Intra_8x8`. Frames at 320x240 and 640x480 hid a budget leak that needed
either a larger frame or a longer clip to cross its ceiling. In each case the
work was verified, reported passing, and wrong.

## 18. Profiling loop, round 3 — the real cost centre, and why round 1's SIMD kernel underperformed

Current state before this round, same 75-frame 3840x2160 Main fixture, measured under background
load (a fuzz sweep and other agents' builds, load average ~7-9):

| | best of 3 |
|---|---|
| vaco decode | 9.89s |
| `ffmpeg -threads 1` | 0.63s |
| ffmpeg, default threads | 0.18s |

**Do not read this against §10's 5.49s or §15's 8.11s as a regression or an improvement** — different
sessions, different machine load, and (for §10 specifically) two rounds of correctness work landed in
between that made the decoder do real work it previously skipped or did wrongly (weighted prediction,
correct deblocking thresholds, `Intra_8x8`, B-slices). Whether that correctness work cost time is a
real, separate, unmeasured question — it needs an interleaved A/B across those specific commits on a
quiet machine, which this round did not attempt.

**Symbolication.** `--unstable-presymbolicate` against the stripped `release` binary resolved almost
nothing this round — most leaf samples came back as bare hex addresses rather than names, worse than
the partial failures §10/§14 describe. Building with `cargo build --profile dist` instead (same
release codegen — `opt-level = 3`, `lto = "fat"`, `codegen-units = 1` — but `strip = "none"` and
`debug = "line-tables-only"`, an existing workspace profile) kept the optimisation and added symbols.
`dsymutil` on that binary plus `llvm-symbolizer --obj=<dSYM> --inlines -f -C -p`, fed each leaf
sample's address (module-relative offset plus the binary's own Mach-O `__TEXT` `vmaddr`, 0x100000000
on this arm64 build — confirmed correct with `dwarfdump --lookup=<addr>` before trusting any output),
recovered the full inline chain per sample. Aggregating self time by each chain's *outermost*
(physically-emitted) frame gives the first reliable whole-decoder profile this document has:

| self time | function |
|---:|---|
| 28.00% | `deblock::boundary_strength` |
| 18.82% | `reconstruct::reconstruct_picture` |
| 11.72% | `reconstruct::sample_luma_block` |
| 11.36% | `deblock::deblock_picture_luma` (gather/scatter, not the strength derivation) |
| 4.31% | `reconstruct_inter_mb::{closure#0}` |
| 3.99% | `cabac_residual::residual_block_cabac` |
| 3.72% | `deblock::deblock_picture_chroma` |
| 3.55% | `vaco_codec_dsp_idct::h264::idct4x4` |
| 2.67% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` (the round-1 SIMD kernel itself) |

### The deblocking microbenchmark-versus-end-to-end discrepancy, closed

§10/11's masked-select deblocking kernel measured 0.31x/0.41x in its own microbenchmark yet only
~2.6% end to end — a gap `docs/core/simd-adoption-measurements.md` Group 8 flagged as needing
explanation. This profile supplies it directly: **the filter kernel itself, `filter_luma_edge`, is
only 2.67% of total self time.** `boundary_strength` — the scalar predicate that decides *what* to
filter, called once per 4x4 block edge before the kernel ever runs — is **28%**, more than ten times
the kernel's own share and the single largest cost centre in the whole decoder. Vectorising the
filter arithmetic could only ever have moved that 2.67%; the 26% figure Group 8 used to bound the
*possible* win included all of `boundary_strength`'s cost as part of "the edge's real cost," most of
which the kernel never touched.

### The actual defect, and the fix

`boundary_strength(mb_edge, p_mb, p_blk, q_mb, q_blk, ref_list0_poc, ref_list1_poc)` is a pure
function of its arguments. `deblock_picture_luma`'s per-edge gather loop derives `p_blk`/`q_blk` from
`row / 4` (`blk_row`) for a vertical edge (`col / 4` for horizontal), then calls
`boundary_strength` once per *row* — 16 calls per edge — even though all four rows sharing one
`blk_row` group compute byte-identical arguments and therefore an identical `bS`. Clause 8.7.2.1
defines one `bS` per 4x4 luma block, not per pixel row; the loop already computed the block index
correctly, it just re-derived the same answer four times per edge instead of once.
`deblock_picture_chroma` has the same shape at `row / 2` (chroma reuses luma's `bS` at half
resolution, so its own redundancy is 2x rather than 4x).

Fixed in all four gather loops (luma vertical/horizontal, chroma vertical/horizontal) by computing
`boundary_strength` once per `blk_row`/`blk_col` group into a small `[u8; 4]` array before the
per-row/column loop, then indexing into it instead of calling the function again. Pure memoisation
of an already-pure function: no `bS` value changes, no table, no new type, `vaco-simd` untouched.

### Measured

Interleaved baseline/candidate (alternating start order each round), 10 independent process
launches, wall clock, on the 4K 75-frame fixture, decode-only (`-c:v rawvideo -f null -`):

**10 of 10 launches favoured the candidate. Ratios: 0.636, 0.844, 0.765, 0.787, 0.895, 0.785, 0.820,
0.850, 0.680, 0.794 — mean ≈0.786 (≈1.27x), median ≈0.791.** Full numbers, the profiling methodology,
and the symbolication cross-check are in `docs/core/simd-adoption-measurements.md` Group 10.

Best-of-3 absolute times after the fix: **vaco 8.14s**, `ffmpeg -threads 1` 0.62s (unchanged),
ffmpeg default threads 0.18s (unchanged). The single-threaded gap narrows from **~15.7x to ~13.1x**
using this round's own before/after numbers (9.89s/0.63s before, 8.14s/0.62s after). Threading
remains the larger untouched factor and is out of scope here, per this document's own §10/§15 notes.

**Byte-exact against `ffmpeg`, unchanged, on all four regression fixtures**: the 4K clip above, a
60-second 1800-frame 1080p `libx264` file (`big.mkv`), and two fresh stock-`libx264` clips (default
encoder settings — B-frames, CABAC, 3 references) at 322x242 and 1024x576.

### What this says about round 1/2's method, in hindsight

Rounds 1 and 2 (§10, §11) profiled correctly but the profile itself was degraded by presymbolication
failure on a stripped binary, which is why `boundary_strength` — sitting inside `deblock_picture_luma`
and `deblock_picture_chroma`'s call graph the whole time — never surfaced as its own line item until
this round's `dsymutil`/`llvm-symbolizer` pass separated it from its callers. The lesson `E2E-GAPS.md`
§14 drew about `vaco-scale`'s `filter_h` ("the cost was bookkeeping, not arithmetic, and a runtime
trip count hid it from the optimiser") has a sibling here: the cost was a *redundant call*, and a
degraded profile hid it inside a caller's aggregate self time instead of attributing it to its own
name. Get the symbols right before concluding a function is cheap.

## 19. Profiling loop, round 4 — re-measured profile, one attempt, zero commits

Re-measured the whole-decoder profile immediately after round 3 landed, same 4K 75-frame Main
fixture, same method (`cargo build --profile dist` with the `patent-encumbered-h264-decode`
feature, private `--target-dir`, `dsymutil` on the resulting binary, `llvm-symbolizer
--obj=<dSYM> --inlines -f -C -p` fed each leaf sample's module-relative address plus the binary's
own `__TEXT` vmaddr `0x100000000`, confirmed correct against `dwarfdump --lookup`, aggregated by
each chain's outermost physically-emitted frame). Symbolication resolved every sample to a real
function name with a full inline chain, not a bare hex address — 9,715 of 10,166 samples (95.6%)
had their leaf frame inside the `vaco` binary itself, and every one of those addresses
symbolicated successfully via the dSYM.

| self time | function |
|---:|---|
| 23.01% | `reconstruct::reconstruct_picture` |
| 15.87% | `reconstruct::sample_luma_block` |
| 12.70% | `deblock::deblock_picture_luma` (gather/scatter, not the strength derivation) |
| 11.26% | `deblock::boundary_strength` |
| 5.37% | `reconstruct_inter_mb::{closure#0}` |
| 4.66% | `cabac_residual::residual_block_cabac` |
| 4.48% | `mb::decode_slice_cabac` |
| 4.27% | `vaco_codec_dsp_idct::h264::idct4x4` |
| 3.92% | `deblock::deblock_picture_chroma` |
| 3.41% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` (round-1's SIMD kernel) |
| 3.38%/3.35% | `H264Decoder::build_frame` / `H264Decoder::send_packet` |

This confirms round 3's fix took effect exactly as intended: `boundary_strength` fell from **28.00%**
(§18's number, pre-fix) to **11.26%** here — still large, but no longer the single dominant cost.
`reconstruct_picture` and `sample_luma_block` moved from 18.82%/11.72% (§18) to 23.01%/15.87% here;
this is **not** those functions getting slower, it is the same arithmetic occupying a larger *share*
of a smaller total once `boundary_strength`'s redundant calls were cut. Absolute per-frame cost of
`reconstruct_picture`/`sample_luma_block` is unchanged by round 3; only the denominator shrank.

### Attempt: merge Cb/Cr chroma inter prediction into one pass — regression, reverted

`reconstruct_picture`'s innermost-frame breakdown (the deepest inline, before folding up to the
outermost physically-emitted frame) put `predict_chroma_inter` at **9.87% of total self time** —
larger than `boundary_strength`'s own remaining leaf share (2.39%) and the single largest named leaf
inside `reconstruct_picture`'s 23.01%. `reconstruct_picture` calls it once per chroma component
(`for comp in [0, 1]`), and each call independently re-derives, per 4x4 luma block: `blk_xy`, the
`mv_blocks` lookup, `ref_idx_l0`/`ref_idx_l1`, `reads_l0`/`reads_l1`, `mv_l0`/`mv_l1`, `cx0`/`cy0`,
and — one level down, inside the per-pixel sampler — the exact same eighth-pel position/fraction
derivation (`int_x`/`frac_x`/`int_y`/`frac_y`, the four bilinear weights). None of that depends on
which component is being predicted, only the plane data and weight table do, so this looked like
round 3's own shape: the same answer computed twice for an identical input.

**Implemented**: `crate::interp::chroma_mc_sample_pair` (shares position/weight derivation, applies
it to two planes), `crate::reconstruct::sample_chroma_point_pair`, and
`predict_chroma_inter_planes` (one pass over the 16 blocks producing `(cb, cr)` together instead of
two single-plane calls), wired into `reconstruct_picture`'s per-macroblock chroma section. Verified
byte-identical to the two-call original by both a direct unit test (`chroma_mc_sample_pair` against
independently reimplemented per-plane arithmetic, every fractional position) and the full
`cargo test -p vaco-codec-h264 --locked` suite, including the byte-exact-against-ffmpeg integration
test.

**Measured**, interleaved baseline/candidate (alternating start order), 10 independent process
launches, wall clock, 4K 75-frame fixture, decode-only:

| round | baseline (s) | candidate (s) | candidate/baseline |
|---:|---:|---:|---:|
| 1 | 7.465 | 7.645 | 1.024 |
| 2 | 7.154 | 7.313 | 1.022 |
| 3 | 7.309 | 10.977 | 1.502 (outlier — a concurrent process spiked load mid-round) |
| 4 | 7.138 | 7.352 | 1.030 |
| 5 | 7.136 | 7.477 | 1.048 |
| 6 | 7.505 | 7.682 | 1.024 |
| 7 | 7.121 | 7.297 | 1.025 |
| 8 | 7.334 | 7.313 | 0.997 |
| 9 | 7.294 | 7.316 | 1.003 |
| 10 | 7.320 | 7.544 | 1.031 |

**Candidate lost 9 of 10 rounds** (one wash at 0.997); excluding the one clear load-outlier round,
every remaining round still shows the candidate 2-5% *slower*, median ratio **1.024** (a ~2.4%
regression), mean 1.071 (pulled up by the outlier). **Reverted** (`git checkout` on the two touched
files, `interp.rs` and `reconstruct.rs` — no commit was made).

**Why the theory didn't pay off, most likely**: the merged function keeps two 8x8 output arrays and
both planes' MV/ref-idx/plane-pointer state live across the same loop body instead of one, which
plausibly increased register pressure or changed inlining/vectorisation decisions LLVM was already
making well for the two separate single-plane calls (which the optimiser could already treat as
independent, non-interfering pieces of straight-line code). This is the same lesson §11 drew from
its own chroma attempt in the opposite direction ("chroma's `.clamp()` is two cheap ops... the guard
branch cost more than it saved") and the same lesson this whole document keeps landing on: reasoning
about redundant computation correctly identifies a *candidate*, not a result — only the interleaved
measurement decides it, and here it decided no.

**Ruled out by this round**: merging per-component chroma inter prediction into a single pass, at
least in this exact shape (computing both planes' full 8x8 output arrays together in one loop over
all 16 4x4 blocks). A narrower version — sharing only the position/fraction derivation without also
merging the two output arrays and outer bookkeeping into one loop body — was not tried and remains
open for a future round if register pressure is confirmed (e.g. via `-Ccodegen-units=1` disassembly
or `llvm-mca`) as the actual cause rather than assumed.

### Same-session ratio against `ffmpeg -threads 1`

Interleaved, alternating start order, 10 independent launches, wall clock, current (post-round-3,
unchanged by this round) binary against `ffmpeg -threads 1`, same 4K 75-frame fixture:

| round | vaco (s) | ffmpeg -threads 1 (s) | ratio |
|---:|---:|---:|---:|
| 1 | 6.977 | 0.607 | 11.499x |
| 2 | 7.006 | 0.576 | 12.157x |
| 3 | 7.062 | 0.610 | 11.577x |
| 4 | 7.047 | 0.575 | 12.255x |
| 5 | 7.043 | 0.582 | 12.106x |
| 6 | 6.967 | 0.576 | 12.093x |
| 7 | 6.951 | 0.605 | 11.486x |
| 8 | 7.031 | 0.579 | 12.152x |
| 9 | 6.991 | 0.583 | 11.997x |
| 10 | 6.947 | 0.575 | 12.091x |

Median ratio **12.09x**, mean **11.94x** — this session's machine was quieter (load average ~3-4)
than round 3's (~6-16), so this number is not comparable to round 3's own ~13.1x as an absolute
regression or improvement; per this document's own repeated caution, only same-session,
interleaved ratios are comparable, and this round made no code change, so the true gap is
unchanged from round 3's landed state.

### Byte-exactness, unchanged

Re-verified on all four regression fixtures after the revert (current `HEAD` state, i.e. round 3's
landed code, unchanged by this round's reverted attempt): the 4K 75-frame `uhd.mp4`, the 60-second
1800-frame 1080p `big.mkv`, and two freshly-encoded stock-`libx264` clips (default encoder
settings — B-frames, CABAC, 3 references) at 322x242 and 1024x576. All four: `sha256` of
`vaco ... -c:v rawvideo -f rawvideo -` matches `ffmpeg ... -f rawvideo -pix_fmt yuv420p -` exactly.

**No commit this round.** The re-measured profile above is the useful output; the attempt is
recorded as a sixth negative result in this document's own series (after round 2's three
interpolation attempts, the deblocking-kernel/chroma-fast-path pair in §10/§11, and now this one).

## 20. Frame threading — landed, byte-exact, and the fixtures have no B frames

The first threading pass on the H.264 decoder. `-threads N` is plumbed end to
end and several pictures now decode concurrently; **output is bit-identical to
the single-threaded decoder at every thread count**, and threading is **off by
default**. Design, determinism argument and memory accounting:
`docs/codec/frame-threading.md`.

### The design, in one paragraph, and the DPB decision

`H264Decoder` splits into a serial half (`split_packet`: parse, CABAC, the DPB,
reference lists, clause 8.2.5 marking, POC, every output-ordering decision) and
a parallel half (`H264FrameTask`: clause 8.4/8.5 reconstruction, clause 8.7
deblocking, the crop into a `Frame`). **The DPB stays entirely on the
coordinating thread** — the brief's own preferred option, and it costs nothing,
because the DPB's *bookkeeping* (POC, `frame_num`, the per-4x4 motion field
`ColocatedField` reads) is final the moment a slice is entropy-decoded, while
only its *samples* need waiting for. Splitting a DPB entry along that line is
what lets the serial half run arbitrarily far ahead of the pixels without
sharing a single mutable byte. Determinism comes from two mechanisms and no
`unsafe`: `FrameRunner::collect` returns results in **dispatch order**, so every
ordering decision is applied in decode order; and `ProgressPicture` publishes a
band by *moving* it into a `OnceLock`, so a task cannot read a sample that has
not been written — `PictureWriter` is neither `Sync` nor `Clone`, `PictureRef`
is read-only, and the compiler is what rules out the race.

### Measured — interleaved, alternating start order, wall clock, median of 5–6 independent launches

4K 75-frame `uhd.mp4`, decode to rawvideo:

| threads | median | speedup |
|---:|---:|---:|
| 1 | 7.694s | 1.00x |
| 2 | 6.278s | 1.23x |
| 4 | 6.087s | 1.26x |
| 8 | 5.908s | 1.30x |

1080p 360-frame stock-`libx264` B-pyramid clip (`-c:v libx264` with every
default: `bframes=3`, `b_pyramid=normal`, `ref=3`):

| threads | median | speedup |
|---:|---:|---:|
| 1 | 10.977s | 1.00x |
| 2 | 6.164s | 1.78x |
| 4 | 5.490s | 2.00x |
| 8 | 5.525s | 1.99x |

Same-session, same-harness comparison against ffmpeg (6 interleaved rounds,
four commands rotated per round):

| fixture | vaco -threads 1 | vaco -threads 4 | ffmpeg -threads 1 | ffmpeg default |
|---|---:|---:|---:|---:|
| `uhd.mp4` (4K, all-P) | 8.133s | 6.687s | 0.705s | 0.189s |
| B-pyramid 1080p | 10.316s | 5.019s | 0.839s | 0.220s |

So against **`ffmpeg -threads 1`**: 11.54x on the 4K clip (9.49x with
`-threads 4`), 12.29x on the B clip (5.98x with `-threads 4`). Against
**default-threaded ffmpeg**: 43.0x → 35.4x on the 4K clip, 47.0x → 22.8x on the
B clip. On B-frame content frame threading closes roughly half the gap to
default ffmpeg; on the 4K clip it closes almost none.

### CPU utilisation, which is the direct measure of what was achieved

`/usr/bin/time -l`, same two fixtures, one launch each, on the binary as it
stood *before* the macroblock array was charged — which is deliberate: the RSS
column is the evidence for that finding, and charging the array does not change
what is allocated, only what is counted.

| threads | 4K all-P: wall / CPU / peak RSS | B-pyramid: wall / CPU / peak RSS |
|---:|---|---|
| 1 | 7.55s / 99% / 2854 MiB | 10.51s / 99% / 990 MiB |
| 2 | 5.95s / 130% / 2858 MiB | 6.11s / 180% / 1021 MiB |
| 4 | 6.02s / **129%** / 3237 MiB | 5.05s / 217% / 1067 MiB |
| 8 | 6.12s / **129%** / 3321 MiB | 4.77s / **219%** / 1145 MiB |

**On the all-P fixture CPU flatlines at 129% at every thread count above one.**
That is 1.3 cores busy no matter how many threads are offered, which is the
two-stage pipeline stated as a measurement rather than as an inference: there is
one picture's reconstruction running and one picture's entropy decode running,
and nothing else can run because nothing else is unblocked. The B-pyramid clip
reaches 2.2 cores and saturates there, which is the parallelism a `bframes=3`
`b_pyramid=normal` GOP actually contains at picture granularity.

### Why those two rows differ, and it is not a bug

**Neither of this project's two large H.264 fixtures contains a single B
frame.** Measured with `ffprobe -show_entries frame=pict_type`:

| fixture | I | P | B |
|---|---:|---:|---:|
| `uhd.mp4` (4K, 75 frames) | 1 | 74 | 0 |
| `big.mkv` (1080p, 1800 frames) | 8 | 1792 | 0 |
| fresh stock `libx264` 1024x576 | 1 | 21 | 28 |

An all-P stream is a serial dependency chain: picture `N + 1` predicts from
picture `N` and nothing else. At **picture** granularity — which is what this
change implements — there is therefore *no* picture-level parallelism to find
on either large fixture, and the only overlap available is the serial half's
work against the parallel half's, a two-stage pipeline whose ceiling is
`1 / (1 - serial_fraction)`. §19's profile puts the serial half (entropy
decoding plus packet handling) at roughly 13%, predicting ~1.15x; measured
1.23–1.30x. The implementation is at its design's ceiling on that content, and
the design is the limit.

**Which means ffmpeg's ~3.7x on the same file (0.705s → 0.189s, measured this
session) is entirely *row*-level frame threading**, not picture-level: its
picture `N + 1` starts reconstructing as soon as picture `N` has published
enough rows to cover the motion-vector reach. That is a specific, measured
correction to the brief's framing — "vaco decodes single-threaded, ffmpeg gains
3.5x from frame threading" is true, but the 3.5x is unreachable by any amount of
whole-picture concurrency on this fixture.

### What row granularity needs, and why it was not folded into this change

The band machinery already supports it (`PictureSpec::with_band_height`/
`with_guard`, `publish_through` per band, `PlaneView::block`'s guard-row fast
path). Two things have to happen first:

1. **Deblocking must become incremental.** `deblock_picture_luma`/`_chroma` are
   whole-picture passes; a row of macroblock row `r - 1` is only final once
   macroblock row `r`'s top-edge filtering has run, so publication has to come
   from an interleaved reconstruct-then-deblock loop.
2. **Reference reads must become block reads.** A banded plane is not one
   allocation, so `sample_luma_block` and `predict_chroma_inter` cannot keep
   taking a flat `&[u8]` — they would fetch a `BlockRef` per partition. Those
   are §19's 15.87% and 9.87% leaves, i.e. the two hottest functions in the
   decoder, and this document already records **five** reverted attempts at
   changing loops of exactly that shape. Landing that rewrite in the same
   commit as the threading scaffolding would have made a regression in either
   one unbisectable against the other.

Safe Rust is *not* the obstacle. Contiguity and progressive publication are
genuinely incompatible under ordinary borrow rules — a writer cannot hold `&mut`
to rows above `R` while a reader holds `&` to rows below `R` of the same
allocation — which is precisely what `ProgressPicture`'s bands exist to solve,
and they solve it. The cost is that a banded plane forces the reader onto a
block API, and that reader is the hot loop. No design here needed `unsafe`, and
none was written; `cargo xtask unsafe-audit` reports clean.

### Byte-exactness, at every thread count

`ffmpeg -v error -i F -map 0:v:0 -f rawvideo -pix_fmt yuv420p - | shasum -a 256`
against `vaco -threads N -i F -map 0:v:0 -c:v rawvideo -f rawvideo - | shasum
-a 256`, N ∈ {1, 2, 4, 8}, on five fixtures: `uhd.mp4`, `big.mkv`, the two fresh
stock-`libx264` encodes (322x242 and 1024x576) and the new B-pyramid 1080p clip.
Every hash matched ffmpeg's. `-threads 0` and `-threads 64` also match;
`-threads abc` is rejected with the reference's own wording.

`big.mkv` — 1800 frames, the best race detector in the corpus — was run **47
times: twelve each at 1, 2 and 4 threads and eleven at 8**, across three
successive binaries as the change landed. No run ever produced a different hash,
and every one matched ffmpeg's.

Three assertions were also added to the repository so this does not depend on a
shell script being re-run (`crates/codec/vaco-codec-h264/tests/frame_threading.rs`):
output invariance at 1/2/3/4/8 threads over a P-only, a B-slice and a
multi-reference fixture; no leaked per-picture budget charge at any thread
count; and a budget too small for the thread count costing speed rather than the
decode. Each was checked by making the corresponding bug on purpose and
confirming the test fails — the leak ceiling in particular started at 4 MiB,
which caught one of the two leaks and silently passed the other.

### Recommendation on the default: leave it off, with three named conditions

Not because it is wrong. It is byte-exact everywhere it was checked, it never
lost a round at any thread count on any fixture, and on stock-`libx264` content
it is a 2x win. Three specific things should be true before it becomes implicit,
and none of them is "more confidence":

1. **Row granularity should land first.** Both large fixtures are serial P
   chains, where the current answer is ~1.25x. Flipping the default now banks
   that number on the content shape that matters most here, and sets the
   expectation, before the mechanism that actually addresses it exists.
2. **The task's budget charge should be exact, not a 2x over-estimate.** A
   default thread count multiplies the footprint by `threads + 1`, and a
   deliberately conservative per-picture charge is fine while the feature is
   opt-in and wrong once it is not — at 4K with 8 threads it is already ~223 MB
   of charge against `Limits::permissive()`'s 1 GiB, and 8K would not fit.
3. **The count itself must not be `ncores`.** A machine-dependent default makes
   the memory ceiling machine-dependent too, and this decoder's whole claim is
   that its output does not depend on the machine. A fixed small bound —
   `min(ncores, 4)`, where the measured curve has already flattened on both
   fixtures — keeps the claim and takes almost all of the win.

### The memory finding, which was not where the type names said it would be

The per-picture accounting first charged the two things that look expensive: the
coded sample planes (12.4 MB at 4K) and the cropped output frame (about the
same). The thing that actually dominates is `SliceStats::macroblocks` —
`MbSummary` is **1,888 bytes**, and a 4K picture has 32,400 macroblocks, so the
array is **59 MiB**, five times the two sample buffers put together. Every
macroblock carries its full residual and its sixteen 4x4 motion blocks.

It had never been charged to any budget (a plain `Vec::push` inside
`decode_slice_cabac`), which cost nothing while it lived for one `send_packet`
call and costs `threads + 1` copies of it once a task holds one. The RSS column
above is the evidence: 2854 MiB at one thread to 3321 MiB at eight, and 8 x 59
MiB is 472 MiB of that ~470 MiB difference. It is charged now.

Charging it honestly then created a second problem worth recording, because the
shape recurs: at 4K, `-threads 8`'s nine-picture window is ~756 MiB against
`Limits::permissive`'s 1 GiB. It fits by margin, not by design, and 8K or a
tighter `-max_alloc` would not. The failure that produces is the wrong shape — a
thread count that silently also means "and do not decode large pictures". So
`split_packet` now finishes pictures until the next one's charge fits, before
allocating anything for it: **memory pressure is backpressure, not a failed
decode**, and `-threads N` is an upper bound on concurrency rather than a
demand. This is the same rule `vaco-sched`'s wires already encode ("a full wire
does not block its producer; it makes the producer unrunnable", and the
empty-wire clause underneath it): never let the mechanism that exists to bound
memory be the thing that stops the pipeline.

### Incidental fix

`fuzz/Cargo.toml`'s `codec-h264` feature did not name `vaco-codec-core` or
`vaco-packet`, both of which `fuzz_targets/h264_decode.rs` imports, so the
target could not build under the mandated `--no-default-features --features
codec-h264` invocation at all — ten compile errors, not a fuzzing failure. Fixed
locally to run the target; not committed, per this dispatch's own instruction
that `fuzz/Cargo.toml` is not mine to commit. The next agent to touch that file
should carry the two `dep:` entries.
