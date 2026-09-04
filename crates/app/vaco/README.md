# vaco

`vaco` is Vaco's public Rust facade. `cargo install vaco` installs exactly
`vvmpeg` and `vvprobe`:

```sh
cargo install vaco
vvmpeg -i input.mp4 output.mkv
vvprobe -show_format -show_streams input.mp4
```

## Library

The facade uses namespaces, rather than a collision-prone prelude. This is a
real API entry point for stream-specifier logic:

```rust
use vaco::core::core::MediaType;

assert_eq!(MediaType::Video.specifier_char(), 'v');
```

Packets use the same bounded allocation model as the rest of the pipeline:

```rust
use vaco::{
    core::{core::Error, limits::{Budget, Limits}},
    media::packet::Packet,
};

fn make_packet() -> Result<Packet, Error> {
    let mut budget = Budget::new(Limits::strict());
    Packet::from_slice(&mut budget, &[0, 0, 1, 0x67])
}

let packet = make_packet()?;
assert_eq!(packet.payload(), &[0, 0, 1, 0x67]);
# Ok::<(), Error>(())
```

Registries expose the components enabled in this build:

```rust
assert!(vaco::registry::demuxer_by_name("mp4").is_some());
```

## Scope, compatibility, and licensing

Vaco is an experimental, clean-room Rust media stack; its ordinary crates
forbid unsafe Rust. `vvmpeg` and `vvprobe` follow parts of the ffmpeg/ffprobe
command model, but compatibility is incomplete across codecs, formats,
filters, protocols, options, and output details. Patent-gated APIs are opt-in.

FFmpeg is a mature, broader C ecosystem; its effective licence depends on the
configured LGPL/GPL components. Vaco is MIT OR Apache-2.0 and does not read
FFmpeg source: it uses published specifications and black-box reference-binary
checks. Same-session CPU-work comparisons show mixed performance, with codec
paths still behind; Vaco is not yet a performance-sensitive production
replacement for FFmpeg.
