#!/usr/bin/env python3
"""Emits the workload-matrix JSON spec that bench.py consumes.

Point VACO_BIN / VACO_PROBE_BIN at a `cargo build --profile dist -p vaco-cli
--features vaco-registry/patent-encumbered-h264-decode,vaco-registry/patent-encumbered-hevc-decode,vaco-registry/patent-encumbered-aac-decode`
(plus the equivalent `-p vaco-probe` build for VACO_PROBE_BIN) binary built to
a *private* --target-dir, and E2E_DIR at a directory containing the fixtures
named below (see planning/PERF-BASELINE.md's fixture table for exactly how
each one was generated with ffmpeg). Usage: `python3 gen_spec.py > spec.json`.
"""
import json
import os

VACO = os.environ.get("VACO_BIN", "./target/dist/vaco")
VACO_PROBE = os.environ.get("VACO_PROBE_BIN", "./target/dist/vaco-probe")
E2E = os.environ.get("E2E_DIR", "./e2e")

jobs = []


def video_decode(name, fixture, rounds=6):
    jobs.append({
        "name": name,
        "rounds": rounds,
        "cmds": {
            "vaco": [VACO, "-threads", "1", "-i", f"{E2E}/{fixture}", "-map", "0:v:0", "-c:v", "rawvideo", "-f", "null", "-"],
            "vaco_default": [VACO, "-i", f"{E2E}/{fixture}", "-map", "0:v:0", "-c:v", "rawvideo", "-f", "null", "-"],
            "ffmpeg_t1": ["ffmpeg", "-y", "-threads", "1", "-i", f"{E2E}/{fixture}", "-map", "0:v:0", "-f", "null", "-"],
            "ffmpeg_default": ["ffmpeg", "-y", "-i", f"{E2E}/{fixture}", "-map", "0:v:0", "-f", "null", "-"],
        },
    })


for size, fname in [("sd_640x480", "h264_sd.mp4"), ("720p", "h264_720p.mp4"),
                     ("1080p", "h264_1080p.mp4"), ("4k_3840x2160", "h264_4k.mp4")]:
    video_decode(f"h264_decode_{size}", fname)

for size, fname in [("sd_640x480", "hevc_sd.mp4"), ("720p", "hevc_720p.mp4"),
                     ("1080p", "hevc_1080p.mp4"), ("4k_3840x2160", "hevc_4k.mp4")]:
    video_decode(f"hevc_decode_{size}", fname)

# decode + scale: 2160p -> 1080p
jobs.append({
    "name": "h264_decode_scale_2160p_to_1080p",
    "rounds": 6,
    "cmds": {
        "vaco": [VACO, "-threads", "1", "-i", f"{E2E}/h264_4k.mp4", "-map", "0:v:0", "-vf", "scale=1920:1080", "-c:v", "rawvideo", "-f", "null", "-"],
        "vaco_default": [VACO, "-i", f"{E2E}/h264_4k.mp4", "-map", "0:v:0", "-vf", "scale=1920:1080", "-c:v", "rawvideo", "-f", "null", "-"],
        "ffmpeg_t1": ["ffmpeg", "-y", "-threads", "1", "-i", f"{E2E}/h264_4k.mp4", "-map", "0:v:0", "-vf", "scale=1920:1080", "-f", "null", "-"],
        "ffmpeg_default": ["ffmpeg", "-y", "-i", f"{E2E}/h264_4k.mp4", "-map", "0:v:0", "-vf", "scale=1920:1080", "-f", "null", "-"],
    },
})

# decode + encode transcode: H.264 1080p -> FFV1 in matroska (a real vaco-implemented encoder)
jobs.append({
    "name": "transcode_h264_to_ffv1_1080p",
    "rounds": 6,
    "cmds": {
        "vaco": [VACO, "-threads", "1", "-i", f"{E2E}/h264_1080p.mp4", "-map", "0:v:0", "-c:v", "ffv1", "-f", "matroska", "/dev/null"],
        "vaco_default": [VACO, "-i", f"{E2E}/h264_1080p.mp4", "-map", "0:v:0", "-c:v", "ffv1", "-f", "matroska", "/dev/null"],
        "ffmpeg_t1": ["ffmpeg", "-y", "-threads", "1", "-i", f"{E2E}/h264_1080p.mp4", "-map", "0:v:0", "-c:v", "ffv1", "-f", "matroska", "/dev/null"],
        "ffmpeg_default": ["ffmpeg", "-y", "-i", f"{E2E}/h264_1080p.mp4", "-map", "0:v:0", "-c:v", "ffv1", "-f", "matroska", "/dev/null"],
    },
})

# remux / copy: mkv -> mp4 stream copy, 60s 1080p
jobs.append({
    "name": "remux_mkv_to_mp4_copy_60s_1080p",
    "rounds": 8,
    "cmds": {
        "vaco": [VACO, "-i", f"{E2E}/big.mkv", "-map", "0:v:0", "-c", "copy", "-f", "mp4", "/dev/null"],
        "ffmpeg": ["ffmpeg", "-y", "-i", f"{E2E}/big.mkv", "-map", "0:v:0", "-c", "copy", "-f", "mp4", "/dev/null"],
    },
})

# audio decode (opus excluded: vaco has no registered Opus decoder at all --
# only a bitstream parser (parse-opus) -- see report for detail; the job
# would just fail every round with "no decoder for the input codec")
for codec, fname in [("aac", "audio_aac.m4a"), ("mp3", "audio_mp3.mp3"),
                      ("flac", "audio_flac.flac")]:
    jobs.append({
        "name": f"audio_decode_{codec}",
        "rounds": 6,
        "cmds": {
            "vaco": [VACO, "-i", f"{E2E}/{fname}", "-map", "0:a:0", "-c:a", "pcm_s16le", "-f", "null", "-"],
            "ffmpeg_t1": ["ffmpeg", "-y", "-threads", "1", "-i", f"{E2E}/{fname}", "-map", "0:a:0", "-f", "null", "-"],
            "ffmpeg_default": ["ffmpeg", "-y", "-i", f"{E2E}/{fname}", "-map", "0:a:0", "-f", "null", "-"],
        },
    })

# probe-only
for fname in ["h264_1080p.mp4", "h264_4k.mp4", "big.mkv"]:
    jobs.append({
        "name": f"probe_{fname.replace('.', '_')}",
        "rounds": 8,
        "cmds": {
            "vaco_probe": [VACO_PROBE, "-show_format", "-show_streams", f"{E2E}/{fname}"],
            "ffprobe": ["ffprobe", "-v", "error", "-show_format", "-show_streams", f"{E2E}/{fname}"],
        },
    })

print(json.dumps(jobs, indent=2))
