#!/bin/bash
# Generate the SMALL fixture set the instruction-count harness uses.
#
# File names deliberately match planning/PERF-BASELINE.md's fixture table so
# that scripts/perf-baseline-gen-spec.py -- the single source of truth for the
# command shapes -- can be pointed at this directory unchanged, via E2E_DIR.
# The CONTENT is much smaller: cachegrind runs ~50-100x slower than native, so a
# 4K/125-frame fixture would take tens of minutes per command.
#
# Everything is generated with the ffmpeg *binary* as a black box (D6/D7), never
# from its source.
#
#   ICOUNT_FIXTURES=/path/to/dir bash scripts/perf-icount-fixtures.sh
set -euo pipefail
DIR="${ICOUNT_FIXTURES:?set ICOUNT_FIXTURES to the output directory}"
FFMPEG="${FFMPEG_BIN:-ffmpeg}"
mkdir -p "$DIR"

gen() { echo "  $1"; "$FFMPEG" -y -loglevel error "${@:2}"; }

# SD video and 30s audio match planning/PERF-BASELINE.md §1's own fixture sizes,
# so an instruction-count ratio can be read beside that table's wall-clock ratio
# for the same workload. The 720p/1080p/4K sizes deliberately have no equivalent
# here: at ~30x cachegrind slowdown a 4K decode is tens of minutes per sample,
# and the ratio it would produce is the SD ratio with more cache pressure the
# simulated cache model does not faithfully reproduce anyway.
gen h264_sd.mp4    -f lavfi -i testsrc2=size=640x480:rate=25:duration=5 \
                   -c:v libx264 -pix_fmt yuv420p "$DIR/h264_sd.mp4"
gen hevc_sd.mp4    -f lavfi -i testsrc2=size=640x480:rate=25:duration=5 \
                   -c:v libx265 -pix_fmt yuv420p "$DIR/hevc_sd.mp4"
# 1080p exists only for the probe jobs, which never decode a frame.
gen h264_1080p.mp4 -f lavfi -i testsrc2=size=1920x1080:rate=25:duration=0.2 \
                   -c:v libx264 -pix_fmt yuv420p "$DIR/h264_1080p.mp4"
gen big.mkv        -f lavfi -i testsrc2=size=640x480:rate=25:duration=10 \
                   -c:v libx264 -preset veryfast -pix_fmt yuv420p "$DIR/big.mkv"
gen audio_aac.m4a  -f lavfi -i sine=frequency=440:duration=30 -c:a aac -b:a 128k "$DIR/audio_aac.m4a"
gen audio_mp3.mp3  -f lavfi -i sine=frequency=440:duration=30 -c:a libmp3lame -b:a 128k "$DIR/audio_mp3.mp3"

echo "fixtures in $DIR:"
ls -la "$DIR"
