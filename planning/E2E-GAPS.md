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

## 21. Row-level frame threading — 129% CPU to 615%, and 1.28x to 4.05x on all-P 4K

§20 landed picture-level frame threading, measured 1.23–1.30x on the 4K all-P
fixture, and diagnosed its own ceiling precisely: **both large fixtures are
serial P chains** (`uhd.mp4` is 1 I + 74 P, `big.mkv` is 8 I + 1792 P), so at
picture granularity there is nothing to overlap and CPU flatlines at 129% at
every thread count above one. This is the row-level answer it named as the next
step. Design: `docs/codec/frame-threading.md`.

Three commits, each byte-exact and each measured on its own, because a
regression in any one of them had to stay bisectable against the others.

### Stage 1 — the filter runs a macroblock row at a time, one row behind

Clause 8.7 ran as two whole-picture sweeps *after* reconstruction, so no row was
final until the whole picture was. `DeblockCtx::luma_mb_row`/`chroma_mb_row` are
the same code addressable a row at a time; `deblock_picture_luma`/`_chroma` stay
as loops over them and as the order-independent oracle the new schedule is
checked against.

Two boundary conditions decide the schedule, and both are exact:

* **The filter lags reconstruction by exactly one macroblock row.** Clause 8.3's
  intra prediction is defined on *unfiltered* neighbours and reads exactly one
  row above the current macroblock row — luma `my * 16 - 1`, chroma
  `my * 8 - 1`. Filtering row `my - 1` rewrites those rows (its vertical edges
  touch all sixteen of its own luma rows, the last of which *is* `my * 16 - 1`),
  so it cannot run before row `my` is reconstructed; it needs nothing from row
  `my`, so one row of lag is also sufficient, and no saved copy of the
  unfiltered top border is needed. A lag-zero schedule would need one.
* **A row is final only once the *next* row has been filtered.** Filtering row
  `d` writes upwards: luma's top macroblock edge rewrites `p0`/`p1`/`p2`, rows
  `d * 16 - 1`, `- 2`, `- 3`; chroma's rewrites `p0` alone, row `d * 8 - 1`. So
  after row `d`, luma rows `..d * 16 + 13` and chroma rows `..d * 8 + 7` are
  final.

**Measured** (interleaved, alternating start order, 10 independent launches,
`-threads 1`, 4K 75-frame fixture, decode only): wall ratios 1.008, 0.990,
1.016, 0.990, 1.002, 1.001, 1.002, 0.990, 1.018, 0.979 — median **1.0013**, mean
0.9996, 4 of 10 rounds to the candidate. Children's user+sys CPU seconds, which
carry far less load noise than wall clock: median ratio 1.0015. **The
restructure is free to within the measurement**, which is the thing that had to
be established before threading could hide it.

### Stage 2 — the two hottest functions read a plane, not a slice

§20 flagged this as the work's first-class regression risk: a banded plane is
not one allocation, so `sample_luma_block` (15.87% of self time, §19) and
`predict_chroma_inter` (9.87%) cannot keep taking a flat `&[u8]`, and this
document already records **five** reverted attempts at changing loops of exactly
that shape.

`RefPlane` has two arms and that is the whole design. `Flat` is one allocation
and reads it with exactly the indexed fetch the decoder has always used, so
`-threads 1` and every test oracle pay *nothing* — the same instructions, not
"almost". `Banded` asks `PlaneView::block` for the region a block needs in one
piece. Both feed the same clause 8.4.2.2 arithmetic in `crate::interp`; only the
fetch closure differs, which is what the in-picture and edge-clamped fetches
already did to each other. The regions are the filters' exact footprints: 9x9 at
`(x0 - 2, y0 - 2)` for luma's six-tap over a 4x4 block, 3x3 for the bilinear
over its four chroma sub-positions.

That last one forced the only arithmetic-adjacent change: `sample_chroma_point`
sampled one point at a time and rebuilt its fetch closure per point, and
`sample_chroma_2x2` returns the 2x2 group instead, because a banded reference
must be asked for their shared region once rather than four times.

**Measured** (same harness, `-threads 1`): wall ratios 0.972, 0.989, 0.969,
0.980, 1.010, 0.979, 0.954, 0.967, 0.960, 0.971 — median **0.9713**, mean
0.9749, **9 of 10 rounds to the candidate**; CPU-seconds median 0.9719. So the
identified first-class risk measured the other way: a **2.9% single-threaded
speedup**, from the chroma closure hoist, before threading is involved at all.

This is worth recording against this document's own six negative results.
Rounds 2, 3 and 4 established that reasoning about redundant computation
identifies a candidate and not a result; here a change made for a *structural*
reason (the reader must accept a banded plane) happened to remove redundant
work as a side effect, and the interleaved measurement is what turned that from
a hope into a number. The method did not change; only the outcome did.

### Stage 3 — publish rows as they become final, wait per macroblock row

* **A DPB entry is banded above one thread**: 32-row bands, 8 guard rows. The
  guard is exact, not a margin — clause 8.4.2.2.1's six-tap reads a **9-row**
  region, and a read of `h` rows straddles a seam exactly when its first row is
  in the `h - 1` rows above it, so 8 guard rows are what make every such read
  land inside one allocation. Seven pushes those reads onto the copy path; nine
  costs memory for nothing. Both halves are pinned by tests in
  `vaco-codec-core`: a 9-row read at *every* row of a band-32/guard-8 plane is
  borrowed, and the same read on a guard-7 plane is copied.
* **`RowPublisher` publishes a band the moment every row it holds is final**,
  using stage 1's two watermarks. `deblock.rs`'s new tests assert that extent
  from both sides — nothing outside `d * 16 - 3 ..= d * 16 + 14` moves, *and*
  row `d * 16 - 3` really does move, so the watermark is tight rather than
  merely safe and the test is not vacuous.
* **`row_reference_reach` derives the wait per macroblock row**, walking that
  row's own motion vectors and reporting, per reference and per plane, the
  deepest row clause 8.4.2.2 will read: `y + (mv_y >> 2) + 6` for luma,
  `cy + (mv_y >> 3) + 2` for chroma. Those are the same numbers the two samplers
  use to size the region they ask for, so the bound cannot drift from the read.
  A reference the row does not predict from is not waited on at all. A read past
  what was waited for is *refused* by `PlaneView::block` and raises an error at
  the end of the row — so a bound that was ever too small is an error, never
  wrong pixels.

`ProgressPlane::band_of` runs on every block read and was a division by a
runtime value; it is a shift now whenever the band height is a power of two,
which `single_band` arranges for itself too.

No new threading mechanism: same `FrameRunner`, same dispatch-order collection,
same `ProgressPicture`, same deadlock argument (tasks leave one queue in
dispatch order and wait only on pictures earlier in decode order, so the
lowest-indexed in-flight task never blocks).

### Measured — scaling and CPU utilisation

Medians of 8 interleaved launches, rotating start order, wall clock, decode
only. CPU utilisation from `/usr/bin/time -l` on separate single launches.

**4K 75-frame all-P `uhd.mp4`** — the fixture §20 could not accelerate:

| threads | wall | speedup | CPU | CPU-seconds |
|---:|---:|---:|---:|---:|
| 1 | 7.020s | 1.00x | 100% | 6.96 |
| 2 | 3.569s | 1.97x | 236% | 8.31 |
| 4 | 2.285s | 3.07x | 447% | 9.97 |
| 8 | 1.907s | 3.68x | 625% | 11.72 |

**360-frame stock-`libx264` B-pyramid 1080p** (`bframes=3`, `b_pyramid=normal`,
`ref=3`; 189 B, 169 P, 2 I):

| threads | wall | speedup | CPU |
|---:|---:|---:|---:|
| 1 | 9.726s | 1.00x | 100% |
| 2 | 5.332s | 1.82x | 220% |
| 4 | 3.241s | 3.00x | 439% |
| 8 | 2.445s | 3.98x | 742% |

Head to head against §20's binary, same session, 8 interleaved rounds, 4K all-P:

| | picture granularity (§20) | row granularity |
|---|---:|---:|
| `-threads 1` | 6.858s | 6.687s |
| `-threads 8` | 5.371s — 1.28x, **129% CPU** | **1.695s — 4.05x, 615% CPU** |

**129% is §20's own diagnosis reproduced exactly**, on the same machine in the
same session, which is what makes the 615% next to it a measurement of the
change rather than of the day.

Above four threads the curve flattens: eight buys another 15–25% for roughly 40%
more CPU-seconds. The rising CPU-seconds column is the honest cost — 6.96 to
11.72 at 4K — and is the banded read path, the guard-row copies, and blocking
and waking on band publication.

### Same-session ratio against ffmpeg

Interleaved across five rotated commands, 6 independent launches, medians:

| fixture | vaco `-threads 1` | vaco `-threads 4` | vaco `-threads 8` | `ffmpeg -threads 1` | ffmpeg default |
|---|---:|---:|---:|---:|---:|
| `uhd.mp4` (4K, all-P) | 6.955s | 2.271s | 1.899s | 0.589s | 0.148s |
| B-pyramid 1080p | 9.213s | 2.870s | 2.210s | 0.830s | 0.167s |

Against **`ffmpeg -threads 1`**: 11.81x on the 4K clip, closing to **3.86x at
four threads and 3.22x at eight** — §20's best on this fixture was 9.49x. On the
B clip 11.10x closing to 3.46x / 2.66x. Against **default-threaded ffmpeg**:
47.0x → 12.8x on the 4K clip, 55.2x → 13.2x on the B clip.

Worth reading alongside: ffmpeg's own default threading takes 0.148s from 0.805
CPU-seconds against 0.589 at one thread — 3.98x wall for 1.37x the CPU. This
decoder gets 3.66x wall for 1.69x the CPU on the same file. The *scaling* is
comparable; the per-thread overhead is not, and that gap is the next thing to
look at if this is revisited.

### Byte-exactness

`ffmpeg -v error -i F -map 0:v:0 -f rawvideo -pix_fmt yuv420p - | shasum -a 256`
against `vaco -threads N -i F -map 0:v:0 -c:v rawvideo -f rawvideo -`, N in
{1, 2, 4, 8}, on `uhd.mp4`, `big.mkv`, the B-pyramid 1080p clip, and fresh stock
`libx264` encodes at 322x242 and 1024x576. Every hash matched ffmpeg's, at every
stage. `-threads 0/3/5/16/64` also match; `-threads abc` is still rejected with
the reference's own wording.

Two fixtures were added for branches this change introduced and the existing
corpus did not reach: a `no-deblock=1` encode, which takes the "a row is final
the moment it is reconstructed" path because `disable_deblocking_filter_idc` is
1 and there is no filter to lag behind, and a `deblock=-3,2` encode, which
exercises non-zero `slice_alpha_c0_offset_div2`/`slice_beta_offset_div2` through
the new per-row `DeblockCtx`. Both byte-exact at 1/2/4/8 threads.

A `slices=4` encode was also tried and is refused outright — "more than one slice
per picture is not supported" — identically on the pre-change binary and on
every stage of this one. That is a pre-existing scope refusal, not a regression,
and it is why `macroblocks_in_raster_order`'s whole-picture fallback has no
reachable input today: it exists so the row schedule cannot be the thing that
breaks when multi-slice support lands.

`big.mkv` — 1800 frames, the best race detector in the corpus — was run **48
times on the final binary: twelve each at 1, 2, 4 and 8 threads**, plus 14 more
on the identical predecessor binary before a doc-comment rebuild. No run ever
produced a different hash, and every one matched ffmpeg's. (§20's own tally on
the same fixture was 47.)

That is the check that matters most and it is worth saying why: a race here
would be a *reader* seeing a band before its rows were final, which shows up as
a handful of wrong pixels in one macroblock row of one frame out of 1800 — the
kind of thing a five-frame fixture will never find and a single run of a long
one might not either.

Three tests were added so the boundary conditions do not depend on a shell
script being re-run:

* `deblock.rs`: filtering one luma macroblock row reaches exactly three rows
  above it, and one chroma row above it — asserted from both sides, so a
  loosened watermark and a broken one both fail.
* `deblock.rs`: the row-by-row schedule and the whole-picture sweep agree.
* `vaco-codec-core`'s `picture.rs`: a 9-row read never straddles a band with an
  8-row guard, and *does* straddle one with a 7-row guard.

`cargo test -p vaco-codec-h264 -p vaco-codec-core --locked` passes (143 and 75
across all their test targets; stage 3's own commit message quotes 114 and 17,
which are the `--lib` and `--test picture` subsets it happened to have open —
the totals above are the ones to read),
`cargo clippy --all-targets -- -D warnings` is clean on both, `cargo xtask
unsafe-audit` reports every crate forbidding unsafe, and `h264_decode` fuzzed
177,672 runs in 91 seconds with no findings.

### The gap this opened, which should be closed before the default moves

**`h264_decode` never calls `set_thread_count`, so the row-progress path has no
fuzz coverage at all** — not the publisher, not the per-row waits, not
`PlaneView::block`'s banded arm. It is covered by the repository's own
invariance tests and by many oracle repetitions, which is not the same thing.
`docs/codec/frame-threading.md`'s "Should it be on by default" now names that as
a condition alongside §20's own two, and it is the cheapest of the three to fix.

### Still not the default

§20's recommendation stands, with its first condition now met. The measured case
for flipping it is far stronger — 3.07x at four threads on the content shape
that dominates this corpus, against 1.26x before — but the per-picture budget
charge is still a deliberate 2x over-estimate, and the threaded path is not
fuzzed. Both are small, specific pieces of work, and neither is "more
confidence".

### Pre-existing, not mine

`cargo xtask dup-check` fails on `CompressionAlgo` (vaco-codec-exr,
vaco-codec-tiff) and `EncodeOptions` (vaco-codec-exr, vaco-codec-jpeg,
vaco-codec-png, vaco-codec-tiff). None of those crates was touched here.

## 22. Threading, final state — verified by the orchestrator

Row-level frame threading is **on by default** at `min(available_parallelism, 4)`.
Measured on this 10-core machine, 4K all-P `uhd.mp4`:

| | time | vs serial | vs `ffmpeg -threads 1` |
|---|---:|---:|---:|
| vaco `-threads 1` | 6.62s | 1.00x | 11.86x |
| **vaco, default** | **1.93s** | **3.43x** | **3.46x** |
| `ffmpeg -threads 1` | 0.56s | | |
| ffmpeg, default | 0.14s | | 13.70x behind |

Byte-exact against ffmpeg on the default path with no `-threads` flag, and at
1/2/4/8/16 on the 4K fixture — checked by me, not only reported.

### Why the default is 4 and not `ncores`

The curve is nearly flat past four (3.37x at 4 against 3.78x at 8 on 4K) for
roughly double the memory, and `ncores` would make the memory ceiling
machine-dependent. Four takes almost all of the win at a bounded, predictable
cost. `-threads N` overrides in both directions; `-threads 1` still forces the
exact serial call sequence. Library callers are unaffected — only the CLI's
resolution changed.

### What made this safe enough to enable

Not the speedup. Three things, in order of how much they mattered:

1. **A determinism fuzz target.** `h264_decode_threaded` derives its thread
   count from the input, decodes the same bytes at 1 thread and at N, and
   asserts the outputs are identical — a race detector, not a panic check. It
   is the only thing exercising band publication, `wait_rows`,
   `PlaneView::block`'s refusal path and `RefPlane::Banded`. 321s, 106,279
   execs, no divergence. Enabling concurrency by default while that path was
   unfuzzed would have been the wrong order, and it was the implementing
   agent that said so.
2. **A tight, two-sided bound.** A row is final only after the next row is
   filtered; the tests assert both that nothing outside the watermark moves
   *and* that the boundary row does — the second half is what stops a
   watermark being vacuously safe. A read past what was waited for is
   **refused**, so a too-small bound is an error, never wrong pixels.
3. **50 runs of the 1800-frame fixture at the default specifically**, zero
   mismatches, on top of 47 and 48 in the two prior passes.

### The stage that was supposed to be the risk

Moving `sample_luma_block` and `predict_chroma_inter` onto a block API was
briefed as the first-class risk, with permission to stop if it cost
single-threaded performance. It measured **2.9% faster**, 9 of 10 rounds:
`RefPlane::Flat` reads with the same instructions as before, and the rewrite
forced hoisting chroma's per-point fetch closure to the 2x2 group a 4x4 block
needs. The refactor paid for itself and found an optimisation.

## 23. AAC IMDCT through `vaco-tx`'s `Plan` (C1) — 217x behind ffmpeg to 2–5x

`vaco-codec-aac`'s production decode path (`reconstruct.rs:447`/`:460`) called
`vaco_tx::reference::imp::imdct` — an `O(n²)` direct evaluation with a `cos`
per `(j, k)` pair, documented in `vaco-tx` as **"verification only"** — while a
fast `Plan`-based transform already existed unused in the same crate. The
perf programme's baseline profile put 80.3% of sampled AAC decode time inside
`libm` under that one function, plus its own 7.7%: ~88% of runtime, and the
worst decode-side ratio against `ffmpeg` in the whole report (217x).

### The change

`reconstruct.rs` now holds an `ImdctPlans` struct (`long`/`short`, one
`Tx<f64>` each) built once with `Plan::<f64>::new(TxKind::Mdct,
Direction::Inverse, n, 1.0, TxFlags::FULL_IMDCT)` for AAC's two fixed block
lengths (2048, 256). `AacDecoder` builds it lazily on first use (its `make`
closure in `DECODER_AAC` is infallible, so the fallible `Plan::new` — which
never actually fails at these fixed lengths — is deferred to `send_packet`,
which already returns a `Result`) and threads `&mut ImdctPlans` through both
`finalize_channel` call sites, reused across every channel and packet since
reconstruction is strictly sequential within one packet. `f64` throughout,
matching the `AGENT-CONSTRAINTS.md`/plan requirement that this be verifiable
against the *current* production output rather than a widened tolerance:
`vaco-tx/tests/oracle.rs`'s `mdct_and_imdct_match_the_reference` already
asserted `Plan::<f64>`'s `FULL_IMDCT` inverse agrees with
`reference::imp::imdct` to `rms_rel < 1e-12` up to `n = 960`, covering AAC's
short length (256) but not its long one — extended to `n = 2048` as part of
this change (still passes at `< 1e-12`). `reference::imdct` itself is
untouched; it remains `vaco-tx`'s own oracle, just no longer reachable from
any production call site (D19 — one definition per concept, one of them now
inert on the runtime path rather than two live ones).

Two small defensive additions at the call site, not present before: the
coefficient buffer fed to `Tx::execute` is `resize`d to the plan's exact
input length before the call. `Tx::execute`'s own contract is "a short
buffer produces no output rather than a panic" in release, but that
"no panic" only holds because of a `debug_assert!` that *does* panic in a
debug/fuzz build on a length mismatch — the old `reference::imdct` never
had that failure mode (a bounded loop over whatever length it was given).
Since `spec.first()` can in principle be `None` (empty windows) whereas the
old code tolerated it silently, padding first keeps the never-panics
property intact end to end rather than depending on `Tx::execute`'s
release-only leniency.

### Verified byte-identical, not just numerically close

Built two `dist` binaries into private `--target-dir`s from a `git worktree`
at the pre-change commit and from the working tree with this change,
features `vaco-registry/patent-encumbered-aac-decode` (H.264/HEVC excluded —
out of this item's lane and, at the pre-change commit, `vaco-registry`
hadn't gained those feature flags yet). Confirmed the "after" binary actually
contains the new code and the "before" binary does not (a `strings` grep for
one of the new error messages), so the comparison is against what it claims
to be, not a stale or misresolved binary.

5 fixtures (`ffmpeg -c:a aac`, generated with `lavfi` `sine` sources):
22050 Hz mono, 44100 Hz mono, 44100 Hz stereo, 48000 Hz stereo, 48000 Hz 5.1
(`channelConfiguration` 6, via `pan=5.1`). AAC-LC only — the only profile
this decoder claims (§"Known gaps": SBR/HE-AAC, #446, is not landed).

```
vaco -i <fixture> -map 0:a:0 -c:a pcm_s16le -f s16le -  | shasum -a 256
```

at each file's native sample rate (no `-ar`/`-ac` override, so no resampler
sits between the decoder and the hash) — all 5 hashes identical before and
after. AAC is not byte-exact against `ffmpeg` (never claimed to be, see
"Decode accuracy" in `docs/codec/vaco-codec-aac.md`) but this change's own
bar is stricter and the one that actually matters here: identical to *itself*
before and after, since `reference::imp::imdct` and the fast `Plan` are
already known to agree to `1e-12` — a differing output sample would mean the
wiring (scale, coefficient count, buffer order) was wrong, not that rounding
differs. It wasn't; every fixture matched exactly.

### Measured — interleaved A/B, alternating start order, median of 6–10 independent launches per fixture

`scripts/perf-baseline-bench.py`, one job per fixture, `vaco_before` /
`vaco_after` / `ffmpeg_t1` interleaved every round. Load average was high for
this whole run (six other agents active; **1-min load 28–44** across the
run, well past the ~8 the measurement protocol calls light) — reported here
per protocol, and a 5-round supplementary check with `/usr/bin/time -l`
CPU-seconds (`user+sys`) on the primary fixture agrees with the wall-clock
numbers to within their own spread, so the load noise did not change the
conclusion:

| Fixture | before median | after median | ffmpeg `-threads 1` median | before/ffmpeg | after/ffmpeg | before/after |
|---|---:|---:|---:|---:|---:|---:|
| mono, 22050 Hz | 3.700s | 0.058s | 0.025s | 147x | 2.3x | 63x |
| mono, 44100 Hz | 8.119s | 0.126s | 0.034s | 242x | 3.8x | 64x |
| stereo, 44100 Hz | 7.569s | 0.124s | 0.038s | 202x | 3.3x | 61x |
| stereo, 48000 Hz | 27.187s | 0.277s | 0.055s | 496x | 5.1x | 98x |
| 5.1, 48000 Hz | 20.157s | 0.204s | 0.045s | 444x | 4.5x | 99x |

Win/loss: **44/44 interleaved rounds** (10+10+6+10+8, one per fixture) had
`vaco_after` faster than `vaco_before` — not a median artefact, every single
round. Supplementary CPU-seconds check (mono 44100 Hz, 5 rounds,
`/usr/bin/time -l`): before ≈ 7.6–8.3s `user+sys`, after ≈ 0.10–0.11s,
`ffmpeg -threads 1` ≈ 0.05s — before/ffmpeg ≈ 156x, after/ffmpeg ≈ 2.1x,
before/after ≈ 74x, consistent with the wall-clock table.

The `audio_decode_aac` job's own fixture (`audio_aac.m4a` — the mono/44100
row above, copied byte-for-byte from `PERF-BASELINE.md`'s corpus) reproduces
that report's 217x-class ratio on the pre-change binary almost exactly
(242x here, under heavier load), which is the sanity check that the "before"
binary really is measuring what the baseline measured, not something else.

### Ceiling: predicted ~8x, measured 61x–99x — exceeded, and why

The baseline's Amdahl ceiling (88% of runtime in the IMDCT, sped up to the
limit) predicted `1/(1-0.88) ≈ 8.3x`, i.e. 217x → ~27x against `ffmpeg`. The
measured before/after speedup is **61x–99x**, roughly 8–12x past that
ceiling. Solving the same formula backwards from the measured speedup puts
the *effective* sequential-remaining share at ~98–99%, not 88%: a sampling
profiler's stack-percentage and a wall-clock-share are related but not
identical measures (inlining, off-CPU time, and sampling granularity at
4000 Hz over a comparatively short per-frame call all bias a hot, tight,
non-inlined leaf like `imp::imdct`'s `cos` loop toward under-attribution
relative to its true wall-clock cost). Practically: at these block sizes
(2048/256) the `O(n²)` reference so thoroughly dominates total runtime that
removing it collapses decode almost to the non-IMDCT remainder (parsing,
Huffman decode, windowing, overlap-add) rather than to the fraction the
profile assigned it. This is a case where the ceiling estimate undershot the
true win rather than the far more common direction in this programme's
history (a confident prediction measuring backwards) — the fix still needed
verifying byte-for-byte, which it did, before trusting the number.

### Files touched (this item's lane only: AAC/transform crates)

- `crates/codec/vaco-codec-aac/src/reconstruct.rs`: `ImdctPlans`, the two
  `finalize_channel` IMDCT call sites rewired from `reference::imdct` to
  `Tx<f64>::execute`.
- `crates/codec/vaco-codec-aac/src/decoder.rs`: `AacDecoder.imdct: Option<ImdctPlans>`,
  lazy construction, threaded into both `finalize_channel` sites.
- `crates/signal/vaco-tx/tests/oracle.rs`: added `n = 2048` to
  `mdct_and_imdct_match_the_reference`'s size list (256 was already covered),
  closing the gap the plan's own stop condition named before trusting the
  production wiring on it.

`vaco-codec-h264`, `vaco-codec-hevc`, the filter crates, `vaco-conformance`
and the fuzz harnesses were not touched — outside this item's lane.

## 24. HEVC B1 — row-wise copies replace five per-sample loops, 1.22–1.29x

PERF-PROGRAMME.md's B1: the baseline's innermost-frame profile put 31.3% of
HEVC decode in five per-sample, bounds-checked copy loops that touch no
arithmetic at all — `write_inter_cu_no_residual`'s `Plane::set` (9.32%),
`Snapshot::capture`'s `Plane::get` (8.08%), `sao::offset_block` (5.35%),
`emit_pocs`'s `u16 -> u8` blit (5.11%), `build_cu_prediction`'s `i32` blit
(3.48%). None of the five change what gets computed, only how many times a
bounds check and an `Option` unwrap run to move it.

### The change

`crate::framebuf::Plane` grows `row`/`row_mut`/`mark_row_ready`/
`clone_samples` (all safe slice operations — no `unsafe`, no
`indexing_slicing`, `#![forbid(unsafe_code)]` untouched):

- `sao::Snapshot::capture` is one `Budget`-charged `Plane::clone_samples`
  call (a single `copy_from_slice` over the whole plane) instead of a
  per-sample `Plane::get` loop. `Snapshot::get` and `Plane::set_i32` are now
  dead and removed.
- `ctu::blit` (one PU's `i32` prediction into its CU-sized buffer) and
  `ctu::write_pred_block` (a CU's finished prediction into the picture) copy
  or clamp-and-convert one row at a time via slices, instead of one
  bounds-checked 2-D index per sample.
- `decoder::blit` (the `u16` reconstruction plane into the `u8` output
  `Frame` at emission) reads a whole plane row via the new `Plane::row`
  instead of `Plane::get` per sample.
- `sao::offset_block` fetches `Snapshot`/`Plane` rows once per row instead of
  re-deriving the 2-D bounds check on every sample (three times per sample
  for an edge-offset class: the sample itself plus its two neighbours), and
  writes through `Plane::row_mut`/`Plane::mark_row_ready` instead of the old
  `Plane::set_i32`.

### Measured — interleaved A/B/ffmpeg, CPU-seconds primary (load average 12–21)

Two `--release` binaries in private `--target-dir`s (`vaco-registry/patent-
encumbered-hevc-decode`), HEAD `e79aed7` as baseline against this change as
candidate, 10 interleaved rounds per fixture, wall clock and `resource
.getrusage(RUSAGE_CHILDREN)` CPU-seconds both recorded (the shared
machine's load average was 12–21 throughout the run, well past the ~8
threshold `AGENT-CONSTRAINTS.md`/the plan's own protocol names for
preferring CPU-seconds):

| fixture | baseline CPU | candidate CPU | speedup | wins (of 10) |
|---|---:|---:|---:|---:|
| 640x480 | 0.485s | 0.376s | 1.29x | 10/10 |
| 1280x720 | 1.442s | 1.155s | 1.25x | 8/10 |
| 1920x1080 | 3.435s | 2.708s | 1.27x | 9/10 |
| 3840x2160 | 9.414s | 7.737s | 1.22x | 10/10 |

37/40 rounds favoured the candidate, and every fixture's median did.
`hevc_4k`'s ratio (0.82 CPU / 0.81 wall) clears the item's own stop
condition (≤ 0.85 median, ≥ 1.18x) with margin. Against a same-session
`ffmpeg -threads 1` on the same fixtures, HEVC 4K decode moves from ~8.1x
behind (baseline) to ~6.6x behind (candidate) — still short of the plan's
~3–3.5x realistic target for the full B1–B3 sequence, as expected: B1 alone
was never meant to close that gap.

### Re-profiled: the copy share actually collapsed

`samply` + `dsymutil` (`dist` profile) on the candidate at 4K, outermost
physically-emitted frame, same convention as the baseline's own §9.2: the
five named functions fell from a combined 31.3% to `write_inter_cu_no_
residual` 2.72%, `sao::offset_block` 3.86%, `emit_pocs` 0.86%, and
`Snapshot::capture`/`build_cu_prediction`'s own blit no longer registering
in the top 30 at all (< 0.33% each) — roughly a 5x cut in absolute
copy-bound time once the ~1.22–1.29x total speedup is accounted for.
`sao::offset_block` is the one that did not collapse to memcpy-class: its
cost is genuine per-sample arithmetic (the band/edge-offset computation),
and row-wise fetching only amortised the *bounds check*, not the work
itself — which is most of why the realised ~1.22–1.29x lands under the
item's 1.36x ceiling (and its own ~1.32x "realistic" estimate) rather than
at it.

### Byte-exactness

`ffmpeg -c:v libx265` with no `-x265-params` at all (the task's own
correctness bar — deblocking, SAO, `cu_qp_delta` and weighted prediction
are all in this path), piped through `shasum -a 256` on both sides, no raw
dumps to disk: 322x242, **300x500 (partial CTU row *and* column)**,
416x240, 640x480, 854x480, 1920x1080 and 3840x2160 with `mandelbrot`/`life`
content (real motion and detail), plus the existing `hevc_{sd,720p,1080p,
4k}.mp4` corpus. Baseline and candidate hash identically to `ffmpeg` and to
each other on all eleven. `tests/oracle.rs::dense_content_is_byte_exact`
and `tests/flat.rs` stay green.

### A live lesson for B2–B4: this crate is not exclusively this lane's right now

`AGENT-CONSTRAINTS.md`'s "one agent at a time in this crate" (the plan's own
B1→B2→B3→B4 sequencing) did not hold during this pass: a concurrent agent
landed `constrained_intra_pred_flag` support (`7191816`) while this item's
`ctu.rs`/`decoder.rs` edits were live in the same working tree, and that
agent's own `git commit -- ctu.rs intra_pred.rs` re-staged the *working
tree's* content for `ctu.rs` — including this item's uncommitted
`blit`/`write_pred_block` rewrite — rather than what it had actually
reviewed and meant to commit. A third commit (`e79aed7`) caught the E0599
break this caused (this item's `write_pred_block` called `Plane::row_mut`/
`Plane::mark_row_ready`, which did not exist yet in the committed
`framebuf.rs`) and reverted just those two functions, correctly leaving the
`constrained_intra_pred_flag` fix alone. This item's own final commit
re-applied `blit`/`write_pred_block` against the post-revert tree and
landed cleanly. Nothing here was this item's own mistake, but the next
agent into this lane should not assume exclusive ownership of
`vaco-codec-hevc` just because the plan says so — verify `git log`/`git
status` immediately before every pathspec commit, not once at the start of
the session.

`vaco-codec-h264`, the AAC/transform crates, the filter crates,
`vaco-conformance` and the fuzz harnesses were not touched — outside this
item's lane.
