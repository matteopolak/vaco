# Encoding limits and passes

The CLI resolves `-frames[:specifier]`, `-vframes`, `-aframes`, and `-dframes`
per output stream. The last matching option wins. Nonpositive limits produce an
empty stream; a limit above the source length ends at ordinary EOF.

Transcoding counts frames after filters and before encoding, preserving delayed
encoder packets. Streamcopy counts compressed packets. Audio limits count audio
frames rather than individual samples. Multiple outputs keep independent limits,
including outputs sharing a source, and complex graph outputs use the same
frame-limit node.

`-pass 0` selects ordinary encoding, `-pass 1` writes statistics, and `-pass 2`
consumes those statistics. `-passlogfile prefix` selects `prefix-N.log`, where
`N` is the global output stream index; the default prefix is `ffmpeg2pass`. Reuse
the same input, output stream ordering, filters, frame limits and
rate settings for both passes. Statistics input is bounded to 64 MiB.

```sh
vvmpeg -i input.y4m -c:v libx264 -b:v 1M -frames:v 100 -pass 1 -passlogfile run -f null -
vvmpeg -i input.y4m -c:v libx264 -b:v 1M -frames:v 100 -pass 2 -passlogfile run output.h264
```

`libx264` and `libx265` implement multipass using installed command-line tools.
Other encoders reject multipass explicitly until they implement the typed codec
contract. Streamcopy rejects nonzero passes. A first-pass log is written only
after that encoder successfully drains; invalid or missing second-pass logs
fail rather than silently falling back to single-pass encoding.

To change option resolution, update `vaco-cli::exec` and its CLI integration
tests. `vaco-cli::pass` owns file I/O and encoder wrapping; `vaco-sched` owns
limits and upstream retirement. `EncoderPass` in `vaco-codec-core` is the
shared contract. Configuration consists of the CLI flags and the installed
encoder tools; no environment variables are added.
