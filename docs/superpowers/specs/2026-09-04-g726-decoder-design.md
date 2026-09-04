# G.726 32 kbit/s Decoder Design

## Goal and scope

Replace the non-conforming IMA-like G.726 stand-in with a clean-room,
decoder-only implementation of ITU-T G.726 at 32 kbit/s. The registered
`g726` and `g726le` decoders accept the repository's existing
headerless, mono, 8 kHz raw streams and produce signed 16-bit PCM. The encoder
continues to refuse by name and remains unregistered.

The codec arithmetic follows G.726 (12/1990), clauses 3 and 4, including the
two-pole/six-zero adaptive predictor and fast/slow log-domain scale-factor
adaptation. The linear output path follows Annex A (11/1994), clauses A.2 and
A.3: omit `COMPRESS`, `EXPAND`, and synchronous coding adjustment, limit `SR`
to a 14-bit two's-complement `SO`, then scale `SO` to the crate's signed
16-bit PCM convention. The limiter includes Corrigendum 1 (05/2005), which
places `SR = 57344` in the wrapped 14-bit branch.

Only the four-bit, 32 kbit/s mode is in scope. Existing `CodecId` values and
the raw demuxers do not carry a bitrate parameter, so registering the other
G.726 rates under the same names would make the interpretation ambiguous.

## Decoder architecture

`src/g726.rs` will be rewritten around one persistent decoder state. The state
contains the G.726 delayed variables from Table 6 and executes the exact
fixed-width operations in clause 4 for each four-bit code:

1. derive the quantized difference through `RECONST` and `ADDA`;
2. calculate the sixth-order zero and second-order pole predictions;
3. update predictor coefficients, reconstructed-signal delays, and quantized
   difference delays;
4. update the fast and slow quantizer scale factors and adaptation-speed
   control; and
5. apply Annex A's corrected `LIMO`, returning the 14-bit result as signed
   16-bit PCM with the mandated left shift.

Arithmetic helpers will name the G.726 blocks they implement and preserve the
specified masking, sign-magnitude, floating representation, and saturation
points. Tables with 32 or more entries will receive provenance rows. No
floating-point arithmetic, `unsafe`, or approximation is used.

The wrapper owns state across every packet in a stream. `flush` restores both
the queue and G.726 reset state. Every input byte yields exactly two samples;
packet PTS is retained and output duration is derived from that exact sample
count. Limits are checked before allocating the interleaved PCM buffer.

## Bit packing and registration

At 32 kbit/s each byte carries two four-bit codes. `g726` consumes the
first code from the high nibble; `g726le` consumes it from the low
nibble. Tests will establish those public-name mappings with ffmpeg-generated
fixtures rather than relying on the misleading old helper names.

The two decoder descriptors are added to `vaco-component.toml`, which remains
the single source for generated registry dispatch and CLI listing. The encoder
descriptors are not registered. Direct construction of either encoder keeps a
specific `Error::Unsupported` response explaining that G.726 encoding is not
implemented.

## Verification

Development starts with failing tests proving the current wrappers refuse a
valid packet. The implemented decoder is then checked at three layers:

- fixed-width block and reset tests cover the corrected Annex A limiter,
  nibble order, exact sample count, state continuity across packet boundaries,
  and flush reset;
- the official ITU-T G.726 Appendix II 32 kbit/s ADPCM sequences provide reset,
  normal, overload, and algorithm-stressing inputs. Appendix II's published
  output words target A-law or mu-law decoding with synchronous coding
  adjustment, not Annex A linear output. The original unrestricted-use Sun
  G.72x decoder reproduces all 16,384 published reset-vector outputs exactly,
  validating the fixed-point state machine; Annex A's corrected uniform-PCM
  limiter is then applied to that reconstruction to produce the linear oracle;
- ffmpeg-generated `g726` and `g726le` fixtures are decoded end to end through
  the real `vaco` CLI. Exact input bytes, output samples, output bytes, PTS,
  duration, and Sun/Annex-A reference PCM are compared. The emitted WAV is
  independently read by ffmpeg to verify the output direction available for
  this decoder-only slice. ffmpeg 9.0.1 is retained as a black-box packing and
  interop reference, not the arithmetic oracle: its linear decode differs from
  the standard/Sun result on 1,760/2,400 short-fixture samples and
  9,027/16,384 Appendix-II samples while producing the correct sample counts.

Registry generation, registry reachability, documentation generation,
provenance, crate tests, strict all-target Clippy, and CLI help/listing are
required before the change is committed. Issue #391 stays open because G.729
and other explicitly deferred codecs remain outside this slice.

## Documentation, configuration, and dependencies

`docs/codec/vaco-codec-adpcm.md` will describe the supported 32 kbit/s mode,
packing names, persistent state, and decoder-only boundary. The generated docs
index and registry are refreshed through `xtask`. `provenance/sources.toml`
declares the base recommendation, Annex A, Corrigendum 1, and Appendix II
archive, plus the original unrestricted-use Sun implementation used as a
secondary conformance reference; `provenance/vaco-codec-adpcm.toml` maps any
large transcribed tables.

There are no environment variables or runtime switches. The implementation
depends only on `vaco-core` and the existing registry/raw-demuxer plumbing.
The Sun source and ffmpeg binary are verification inputs only, not build or
runtime dependencies.
