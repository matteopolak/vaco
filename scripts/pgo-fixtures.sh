#!/bin/sh
# Generate the disposable T0/T1 media used by profile/workload.toml.
# FFmpeg is invoked only as a black-box fixture generator; its source is never
# read. The output directory is caller-owned and must not be committed.
set -eu

OUT=${VACO_PROFILE_FIXTURES:?set VACO_PROFILE_FIXTURES to an output directory}
FFMPEG=${FFMPEG_BIN:-ffmpeg}
mkdir -p "$OUT"

"$FFMPEG" -y -loglevel error -f lavfi -i testsrc2=size=320x240:rate=25:duration=0.01 \
    -c:v libvpx-vp9 -pix_fmt yuv420p "$OUT/vp9.webm"
"$FFMPEG" -y -loglevel error -f lavfi -i testsrc2=size=320x240:rate=25:duration=0.25 \
    -c:v libvpx -pix_fmt yuv420p -f ivf "$OUT/vp8.ivf"
"$FFMPEG" -y -loglevel error -f lavfi -i sine=frequency=440:duration=2 \
    -c:a flac -f ogg "$OUT/flac.ogg"
"$FFMPEG" -y -loglevel error -f lavfi -i sine=frequency=440:sample_rate=48000:duration=3 \
    -c:a pcm_s16le "$OUT/pcm.wav"
"$FFMPEG" -y -loglevel error -f lavfi -i testsrc2=size=320x240:rate=25:duration=0.05 \
    -c:v mjpeg -q:v 3 "$OUT/sample.mkv"
cp "$OUT/sample.mkv" "$OUT/truncated.mkv"
truncate -s -128 "$OUT/truncated.mkv"
echo "PGO fixtures written to $OUT"
