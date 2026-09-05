#!/usr/bin/env bash
# Black-box calibration checks for container behaviours the specifications leave open.
#
# Usage: scripts/verify-format-calibrations.sh [all|P1|P2|P3|P4|P5|P6|P7|T1|T2|T3|T4|T5|S1|M1|M3|M5|M7|K2|K3|K4|A1|N1]
#
# Every case creates its own media in a private temporary directory and invokes
# only ffmpeg/ffprobe binaries. A missing binary or rejected command is a
# failure: a calibration that did not run must never look like a passing one.

set -euo pipefail
export LC_ALL=C

case_id=${1:-all}
case_dir=$(mktemp -d /private/tmp/vaco-format-calibrations.XXXXXX)
trap 'rm -rf "$case_dir"' EXIT

require_reference() {
    command -v ffmpeg >/dev/null
    command -v ffprobe >/dev/null
}

probe_value() {
    local file=$1
    local field=$2
    local selector=${3:-}
    if [[ -n "$selector" ]]; then
        local stream_field=${field#stream=}
        # MPEG-TS exposes the selected stream both under its program and at
        # top level. The top-level flat key is the stream observation; taking
        # every rendered value would falsely look like two input streams.
        ffprobe -v error -select_streams "$selector" -show_entries "$field" -of flat "$file" \
            | awk -F= -v key="streams.stream.0.${stream_field}" '$1 == key { gsub(/"/, "", $2); print $2 }'
    else
        ffprobe -v error -show_entries "$field" -of default=noprint_wrappers=1:nokey=1 "$file"
    fi
}

assert_equal() {
    local actual=$1
    local expected=$2
    local context=$3
    if [[ "$actual" != "$expected" ]]; then
        printf 'FAIL %s: expected %q, got %q\n' "$context" "$expected" "$actual" >&2
        exit 1
    fi
}

run_p1() {
    local media="$case_dir/p1-mangled.bin"
    local ready="$case_dir/p1-port"
    local log="$case_dir/p1-ffprobe.log"
    local port
    local server
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v libvpx -f webm "$media"
    # Remove the EBML magic so the content probe contributes zero confidence.
    printf '\000\000\000\000' | dd of="$media" bs=1 seek=0 conv=notrunc status=none

    # This helper serves exactly one localhost request, with a deliberate MIME
    # type. It is plumbing, not an oracle: ffprobe is the only reference binary.
    python3 -c 'from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import sys
payload = Path(sys.argv[1]).read_bytes()
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "video/webm")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    def log_message(self, format, *args):
        pass
server = HTTPServer(("127.0.0.1", 0), Handler)
Path(sys.argv[2]).write_text(str(server.server_address[1]))
server.timeout = 2
server.handle_request()' "$media" "$ready" &
    server=$!
    for _ in {1..40}; do
        [[ -s "$ready" ]] && break
        sleep 0.05
    done
    if [[ ! -s "$ready" ]]; then
        wait "$server" || true
        printf 'FAIL P1: localhost MIME server did not become ready\n' >&2
        exit 1
    fi
    port=$(<"$ready")
    if ffprobe -v debug -show_entries format=probe_score \
        "http://127.0.0.1:${port}/media.bin" > /dev/null 2> "$log"; then
        printf 'FAIL P1: malformed EBML unexpectedly parsed\n' >&2
        exit 1
    fi
    wait "$server" || true
    grep -Fq 'Probing matroska,webm score:0 increased to 30 due to MIME type' "$log"
    grep -Fq 'Format matroska,webm probed with size=2048 and score=30' "$log"
}

run_p2() {
    local payload="$case_dir/p2-payload.aac"
    local found="$case_dir/p2-found.bin"
    local missing="$case_dir/p2-missing.bin"
    local log="$case_dir/p2-debug.log"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i sine=frequency=440:duration=1 -c:a aac -f adts "$payload"
    ffprobe -v debug -show_entries format=format_name,probe_score \
        -of default=noprint_wrappers=1 "$payload" > /dev/null 2> "$log"
    grep -Fq 'Format aac probed with size=2048 and score=51' "$log"

    # A raw ADTS header at byte 1,044,480 is still in the default search
    # window; moving it exactly to byte 1,048,576 makes probing fail.
    dd if=/dev/zero of="$found" bs=1 count=1044480 status=none
    cat "$payload" >> "$found"
    dd if=/dev/zero of="$missing" bs=1 count=1048576 status=none
    cat "$payload" >> "$missing"
    assert_equal "$(probe_value "$found" format=format_name)" "aac" \
        "P2 format at last accepted offset"
    assert_equal "$(probe_value "$found" format=probe_score)" "25" \
        "P2 score at last accepted offset"
    if ffprobe -v error -show_entries format=format_name,probe_score "$missing" > /dev/null 2>&1; then
        printf 'FAIL P2: syncword at one MiB unexpectedly probed\n' >&2
        exit 1
    fi
}

run_p3() {
    local media="$case_dir/p3.mkv"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:d=1 \
        -c:v mpeg4 -f matroska "$media"
    assert_equal "$(probe_value "$media" format=probe_score)" "100" "P3 auto probe_score"
    assert_equal "$(ffprobe -v error -f matroska -show_entries format=probe_score \
        -of default=noprint_wrappers=1:nokey=1 "$media")" "0" "P3 forced matroska probe_score"
}

run_p4() {
    local second="$case_dir/p4-second.ts"
    local frames first combined rate
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc2=size=64x48:rate=30 -frames:v 60 \
        -c:v mpeg2video -f mpegts "$second"
    for frames in 4 5; do
        first="$case_dir/p4-first-${frames}.ts"
        combined="$case_dir/p4-combined-${frames}.ts"
        ffmpeg -hide_banner -loglevel error \
            -f lavfi -i testsrc=size=64x48:rate=25 -frames:v "$frames" \
            -c:v mpeg2video -f mpegts "$first"
        cat "$first" "$second" > "$combined"
        rate=$(probe_value "$combined" stream=r_frame_rate v:0)
        if [[ "$frames" == 4 ]]; then
            assert_equal "$rate" "30/1" "P4 four-frame initial cadence"
        else
            assert_equal "$rate" "150/1" "P4 five-frame initial cadence"
        fi
    done
}

run_p7() {
    local offset media video_duration
    for offset in 7 8; do
        media="$case_dir/p7-offset-${offset}.ts"
        video_duration=$((offset + 1))
        ffmpeg -hide_banner -loglevel error \
            -f lavfi -i "testsrc=size=64x48:rate=25:duration=${video_duration}" \
            -itsoffset "$offset" -f lavfi -i sine=frequency=440:duration=1 \
            -map 0:v -map 1:a -c:v mpeg2video -c:a mp2 \
            -program program_num=1:st=0 -program program_num=2:st=1 \
            -f mpegts "$media"
        if [[ "$offset" == 7 ]]; then
            assert_equal "$(probe_value "$media" stream=codec_name a:0)" "mp2" \
                "P7 seven-second late stream codec"
            assert_equal "$(probe_value "$media" stream=start_time a:0)" "8.429089" \
                "P7 seven-second late stream start"
            assert_equal "$(probe_value "$media" stream=duration a:0)" "1.018778" \
                "P7 seven-second late stream duration"
        else
            # The PMT still declares program 2, but no audio PES arrived in
            # the default analysis window, so its stream remains generic.
            assert_equal "$(probe_value "$media" stream=codec_name a:0)" "mp3" \
                "P7 eight-second late stream generic codec"
            assert_equal "$(probe_value "$media" stream=start_time a:0)" "1.440000" \
                "P7 eight-second generic start"
            assert_equal "$(probe_value "$media" stream=duration a:0)" "9.000000" \
                "P7 eight-second generic duration"
        fi
    done
}

run_p5() {
    local rate expected media actual
    local -a rates=(24000/1001 30000/1001 60000/1001 120000/1001)
    local -a expected_rates=(24000/1001 30000/1001 19001/317 29011/242)
    for index in "${!rates[@]}"; do
        rate=${rates[$index]}
        expected=${expected_rates[$index]}
        media="$case_dir/p5-${rate//\//_}.mkv"
        ffmpeg -hide_banner -loglevel error \
            -f lavfi -i "testsrc=size=64x48:rate=${rate}:d=2" \
            -c:v mpeg4 -f matroska "$media"
        actual=$(probe_value "$media" stream=r_frame_rate v:0)
        assert_equal "$actual" "$expected" "P5 ${rate} r_frame_rate"
    done
}

run_p6() {
    local mp4="$case_dir/p6.mp4"
    local matroska="$case_dir/p6.mkv"
    local ts="$case_dir/p6.ts"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25 -frames:v 1 -c:v mpeg4 -f mp4 "$mp4"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25 -frames:v 1 -c:v mpeg4 -f matroska "$matroska"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25 -frames:v 1 -c:v mpeg2video -f mpegts "$ts"
    assert_equal "$(probe_value "$mp4" stream=r_frame_rate v:0)" "25/1" "P6 MP4 r_frame_rate"
    assert_equal "$(probe_value "$matroska" stream=r_frame_rate v:0)" "25/1" "P6 Matroska r_frame_rate"
    assert_equal "$(probe_value "$ts" stream=r_frame_rate v:0)" "50/1" "P6 MPEG-TS r_frame_rate"
}

run_t2() {
    local media="$case_dir/t2.mp4"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i sine=frequency=440:duration=1 \
        -itsoffset 0.041708 -f lavfi -i testsrc=size=64x48:rate=24:duration=1 \
        -map 0:a -map 1:v -c:a aac -c:v mpeg4 -shortest -movflags +faststart "$media"
    assert_equal "$(probe_value "$media" format=start_time)" "0.000000" "T2 format start_time"
    assert_equal "$(probe_value "$media" stream=start_time a:0)" "0.000000" "T2 audio start_time"
    assert_equal "$(probe_value "$media" stream=start_time v:0)" "0.041667" "T2 video start_time"
}

run_t1() {
    local media="$case_dir/t1-wrap.ts"
    local seek="$case_dir/t1-seek.csv"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v mpeg2video -output_ts_offset 95443 -mpegts_copyts 1 \
        -f mpegts "$media"

    # Confirm the source bytes contain an actual modulo-2^33 PTS decrease,
    # rather than only a large decoded timestamp presented by a helper tool.
    python3 -c 'from pathlib import Path
import sys
raw = Path(sys.argv[1]).read_bytes()
values = []
for offset in range(0, len(raw) - 188 + 1, 188):
    packet = raw[offset:offset + 188]
    if packet[0] != 0x47 or not (packet[1] & 0x40):
        continue
    adaptation = (packet[3] >> 4) & 0x03
    if adaptation not in (1, 3):
        continue
    payload = 4 + (1 + packet[4] if adaptation == 3 else 0)
    if packet[payload:payload + 4] != b"\x00\x00\x01\xe0":
        continue
    if payload + 14 > len(packet) or not (packet[payload + 7] & 0x80):
        continue
    pts = packet[payload + 9:payload + 14]
    values.append((((pts[0] >> 1) & 7) << 30) | (pts[1] << 22) | ((pts[2] >> 1) << 15) | (pts[3] << 7) | (pts[4] >> 1))
if len(values) < 2 or not any(later < earlier for earlier, later in zip(values, values[1:])):
    raise SystemExit("T1 fixture did not cross the 33-bit PTS boundary")' "$media"
    assert_equal "$(probe_value "$media" format=start_time)" "-0.717689" \
        "T1 normalized format start_time"
    assert_equal "$(probe_value "$media" format=duration)" "1.000000" \
        "T1 normalized format duration"
    ffprobe -v error -read_intervals '0.5%+0.2' -select_streams v:0 \
        -show_entries packet=pts_time,dts_time -of csv=p=0 "$media" > "$seek"
    assert_equal "$(head -n 1 "$seek")" "0.242311,0.202311," \
        "T1 post-wrap seek packet"
}

run_t3() {
    local original="$case_dir/t3-original.mp4"
    local patched="$case_dir/t3-patched.mp4"
    local mvhd
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=12 \
        -c:v mpeg4 -movflags +faststart "$original"
    cp "$original" "$patched"
    mvhd=$(LC_ALL=C grep -abo 'mvhd' "$patched" | head -n 1 | cut -d: -f1)
    if [[ -z "$mvhd" ]]; then
        printf 'FAIL T3: generated MP4 has no mvhd box\n' >&2
        exit 1
    fi

    # In a version-0 mvhd, duration is 20 bytes after the four-byte type.
    # The generated movie timescale is 1000, so this changes mvhd to 10 s
    # without changing the 12 s track media duration.
    printf '\000\000\047\020' | dd of="$patched" bs=1 seek="$((mvhd + 20))" \
        conv=notrunc status=none
    assert_equal "$(xxd -p -s "$((mvhd + 20))" -l 4 "$patched")" \
        "00002710" "T3 patched mvhd duration"
    assert_equal "$(probe_value "$patched" format=duration)" "12.000000" \
        "T3 format duration follows longest track"
    assert_equal "$(probe_value "$patched" stream=duration v:0)" "12.000000" \
        "T3 track duration remains twelve seconds"
}

run_t4() {
    local complete="$case_dir/t4-full.ts"
    local two_packets="$case_dir/t4-minus-two.ts"
    local three_packets="$case_dir/t4-minus-three.ts"
    local size
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=2 \
        -c:v mpeg2video -f mpegts "$complete"
    size=$(wc -c < "$complete" | tr -d '[:space:]')
    head -c "$((size - 2 * 188))" "$complete" > "$two_packets"
    head -c "$((size - 3 * 188))" "$complete" > "$three_packets"
    assert_equal "$(probe_value "$complete" format=duration)" "2.000000" \
        "T4 full duration"
    assert_equal "$(probe_value "$two_packets" format=duration)" "2.000000" \
        "T4 two TS packets removed"
    assert_equal "$(probe_value "$three_packets" format=duration)" "1.960000" \
        "T4 three TS packets removed"
}

run_t5() {
    local input="$case_dir/t5-input.mp4"
    local auto="$case_dir/t5-auto.ts"
    local disabled="$case_dir/t5-disabled.ts"
    local zero="$case_dir/t5-make-zero.ts"
    local non_negative="$case_dir/t5-make-non-negative.ts"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v mpeg4 -bf 2 -movflags +faststart "$input"
    ffmpeg -hide_banner -loglevel error -i "$input" -c copy \
        -avoid_negative_ts auto -f mpegts "$auto"
    ffmpeg -hide_banner -loglevel error -i "$input" -c copy \
        -avoid_negative_ts disabled -f mpegts "$disabled"
    ffmpeg -hide_banner -loglevel error -i "$input" -c copy \
        -avoid_negative_ts make_zero -f mpegts "$zero"
    ffmpeg -hide_banner -loglevel error -i "$input" -c copy \
        -avoid_negative_ts make_non_negative -f mpegts "$non_negative"

    # This MPEG-TS muxer shifts auto, make_zero, and make_non_negative alike;
    # disabled leaves the B-frame DTS origin 40 ms earlier. The byte comparison
    # ensures the timestamp observation is an output policy, not display noise.
    cmp -- "$auto" "$zero"
    cmp -- "$auto" "$non_negative"
    if cmp -s -- "$auto" "$disabled"; then
        printf 'FAIL T5: disabled unexpectedly matched auto bytes\n' >&2
        exit 1
    fi
    assert_equal "$(ffprobe -v error -select_streams v:0 \
        -show_entries packet=pts_time,dts_time -of csv=p=0 "$auto" | head -n 1)" \
        "1.440000,1.400000," "T5 auto first packet"
    assert_equal "$(ffprobe -v error -select_streams v:0 \
        -show_entries packet=pts_time,dts_time -of csv=p=0 "$disabled" | head -n 1)" \
        "1.400000,1.360000," "T5 disabled first packet"
}

run_s1() {
    local media="$case_dir/s1-late.ts"
    local sent="$case_dir/s1-sent"
    local stderr="$case_dir/s1-stderr"
    local total
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=2 \
        -c:v mpeg2video -output_ts_offset 3600 -mpegts_copyts 1 \
        -f mpegts "$media"
    assert_equal "$(ffprobe -v error -read_intervals '3600%+0.2' -select_streams v:0 \
        -show_entries packet=pts_time,dts_time -of csv=p=0 "$media" | head -n 1)" \
        "3600.040000,3600.000000," "S1 seekable post-hour packet"
    total=$(wc -c < "$media" | tr -d '[:space:]')
    if python3 -c 'from pathlib import Path
import os, sys
payload = Path(sys.argv[1]).read_bytes()
sent = 0
try:
    while sent < len(payload):
        sent += os.write(sys.stdout.fileno(), payload[sent:sent + 188])
except BrokenPipeError:
    pass
Path(sys.argv[2]).write_text(str(sent))' "$media" "$sent" \
        | ffprobe -v error -read_intervals '3600%+0.2' -select_streams v:0 \
            -show_entries packet=pts_time,dts_time -of csv=p=0 pipe:0 \
            > /dev/null 2> "$stderr"; then
        printf 'FAIL S1: unseekable interval unexpectedly succeeded\n' >&2
        exit 1
    fi
    assert_equal "$(<"$sent")" "$total" "S1 bytes consumed before refusal"
    grep -Fq 'Could not seek to position 3600000000' "$stderr"
    grep -Fq 'Could not read packets in interval' "$stderr"
}

run_m7() {
    local complete="$case_dir/m7-full.mp4"
    local truncated="$case_dir/m7-half.mp4"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v mpeg4 -movflags +faststart "$complete"
    local size
    size=$(wc -c < "$complete")
    head -c "$((size / 2))" "$complete" > "$truncated"
    assert_equal "$(probe_value "$truncated" stream=nb_frames v:0)" "25" "M7 table nb_frames"
    # `-count_packets` reads only packets whose sample bytes remain available;
    # the table count above must not be confused with this recoverable subset.
    assert_equal "$(ffprobe -v error -count_packets -select_streams v:0 \
        -show_entries stream=nb_read_packets -of flat "$truncated" \
        | awk -F= '$1 == "streams.stream.0.nb_read_packets" { gsub(/"/, "", $2); print $2 }')" \
        "13" "M7 readable packets after half mdat"
}

run_m3() {
    local original="$case_dir/m3-original.mp4"
    local patched="$case_dir/m3-rate-two.mp4"
    local original_packets="$case_dir/m3-original.csv"
    local patched_packets="$case_dir/m3-rate-two.csv"
    local elst
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=2 \
        -c:v mpeg4 -movflags +faststart "$original"
    cp "$original" "$patched"
    elst=$(LC_ALL=C grep -abo 'elst' "$patched" | head -n 1 | cut -d: -f1)
    if [[ -z "$elst" ]]; then
        printf 'FAIL M3: generated MP4 has no edit list\n' >&2
        exit 1
    fi

    # Version-0 elst places the 16.16 media_rate twenty bytes after its type.
    # Change the generated rate from 1.0 to 2.0 while retaining every sample.
    printf '\000\002' | dd of="$patched" bs=1 seek="$((elst + 20))" \
        conv=notrunc status=none
    assert_equal "$(xxd -p -s "$((elst + 20))" -l 4 "$patched")" \
        "00020000" "M3 patched elst media_rate"
    ffprobe -v error -show_entries packet=pts_time,dts_time,duration_time -of csv=p=0 \
        "$original" > "$original_packets"
    ffprobe -v error -show_entries packet=pts_time,dts_time,duration_time -of csv=p=0 \
        "$patched" > "$patched_packets"
    assert_equal "$(wc -l < "$patched_packets" | tr -d '[:space:]')" "50" "M3 packet count"
    cmp -- "$original_packets" "$patched_packets"
    assert_equal "$(probe_value "$patched" format=duration)" "2.000000" \
        "M3 rate-two format duration"
}

run_m5() {
    local first="$case_dir/m5-a.mp4"
    local second="$case_dir/m5-b.mp4"
    local -a mux_args=(
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1
        -c:v mpeg4 -movflags +faststart -fflags +bitexact
        -encryption_scheme cenc-aes-ctr
        -encryption_key 00112233445566778899aabbccddeeff
        -encryption_kid 11223344556677889900aabbccddeeff
    )
    ffmpeg -hide_banner -loglevel error "${mux_args[@]}" "$first"
    ffmpeg -hide_banner -loglevel error "${mux_args[@]}" "$second"

    # CENC is present only if its protection scheme and per-sample boxes exist.
    # Fixed key/KID input plus bitexact mode makes this reference mux byte-stable.
    grep -Faq cenc "$first"
    grep -Faq schm "$first"
    grep -Faq tenc "$first"
    grep -Faq senc "$first"
    cmp -- "$first" "$second"
    assert_equal "$(probe_value "$first" stream=nb_frames v:0)" "25" \
        "M5 encrypted table frame count"
}

run_m1() {
    local media="$case_dir/m1-chunks.mp4"
    local order="$case_dir/m1-order.csv"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -itsoffset 1 -f lavfi -i sine=frequency=440:duration=1 \
        -map 0:v -map 1:a -c:v mpeg4 -c:a aac "$media"
    ffprobe -v error -show_entries packet=stream_index -of csv=p=0 "$media" > "$order"
    assert_equal "$(wc -l < "$order" | tr -d '[:space:]')" "70" "M1 packet count"
    assert_equal "$(awk '$1 == 0 { count++ } $1 == 1 { exit } END { print count + 0 }' "$order")" \
        "25" "M1 video packet prefix"
    assert_equal "$(awk '$1 == 1 { count++ } END { print count + 0 }' "$order")" \
        "45" "M1 audio packet suffix"
    assert_equal "$(awk 'NR == 1 { prior = $1; next } $1 != prior { count++; prior = $1 } END { print count + 0 }' "$order")" \
        "1" "M1 stream-index transition count"
}

run_k4() {
    local first="$case_dir/k4-a.mkv"
    local second="$case_dir/k4-b.mkv"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:d=1 \
        -c:v mpeg4 -fflags +bitexact "$first"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:d=1 \
        -c:v mpeg4 -fflags +bitexact "$second"
    cmp -- "$first" "$second"
}

run_k2() {
    local original="$case_dir/k2-original.mkv"
    local patched="$case_dir/k2-duration.mkv"
    local duration
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v mpeg4 "$original"
    cp "$original" "$patched"
    duration=$(grep -abo $'\x44\x89' "$patched" | head -n 1 | cut -d: -f1)
    if [[ -z "$duration" ]]; then
        printf 'FAIL K2: generated Matroska has no Info Duration element\n' >&2
        exit 1
    fi

    # The generated Duration has an eight-byte payload (44 89 88). Replace it
    # with the big-endian IEEE-754 encoding of 12345.6789 Matroska ticks.
    printf '\100\310\034\326\346\061\370\241' | dd of="$patched" bs=1 \
        seek="$((duration + 3))" conv=notrunc status=none
    assert_equal "$(xxd -p -s "$((duration + 3))" -l 8 "$patched")" \
        "40c81cd6e631f8a1" "K2 patched duration bytes"
    assert_equal "$(probe_value "$patched" format=duration)" "12.345678" \
        "K2 duration float truncation"
}

run_k3() {
    local media="$case_dir/k3.mkv"
    local output="$case_dir/k3-tags.txt"
    local segment
    local tags
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -c:v mpeg4 -metadata title=OUTER "$media"
    segment=$(grep -abo $'\x18\x53\x80\x67' "$media" | head -n 1 | cut -d: -f1)
    tags=$(grep -abo $'\x12\x54\xc3\x67' "$media" | tail -n 1 | cut -d: -f1)
    if [[ -z "$segment" || -z "$tags" ]]; then
        printf 'FAIL K3: generated Matroska lacks Segment or Tags\n' >&2
        exit 1
    fi

    # Insert a nested SimpleTag into a generated Tags master. Both enclosing
    # finite EBML sizes grow by the inserted Tag's exact byte count.
    python3 -c 'from pathlib import Path
import sys
def elem(identifier, payload):
    if len(payload) >= 127:
        raise ValueError("test element too large")
    return identifier + bytes([0x80 | len(payload)]) + payload
nested = elem(bytes.fromhex("67c8"), elem(bytes.fromhex("45a3"), b"CHILD") + elem(bytes.fromhex("4487"), b"VALUE"))
parent = elem(bytes.fromhex("67c8"), elem(bytes.fromhex("45a3"), b"PARENT") + nested)
tag = elem(bytes.fromhex("7373"), parent)
path = Path(sys.argv[1])
raw = path.read_bytes()
segment, tags = map(int, sys.argv[2:])
old_tags = ((raw[tags + 4] & 0x3f) << 8) | raw[tags + 5]
new_tags = old_tags + len(tag)
old_segment = int.from_bytes(raw[segment + 4:segment + 12], "big") & ((1 << 56) - 1)
new_segment = old_segment + len(tag)
raw = raw[:segment + 4] + (new_segment | (1 << 56)).to_bytes(8, "big") + raw[segment + 12:]
raw = raw[:tags + 4] + bytes([0x40 | (new_tags >> 8), new_tags & 0xff]) + raw[tags + 6:]
insert_at = tags + 6
path.write_bytes(raw[:insert_at] + tag + raw[insert_at:])' "$media" "$segment" "$tags"
    assert_equal "$(xxd -p -c 34 -s "$((tags + 6))" -l 34 "$media")" \
        "73739f67c89c45a386504152454e5467c89045a3854348494c4444878556414c5545" \
        "K3 nested SimpleTag bytes"
    ffprobe -v error -show_entries format_tags -of default=noprint_wrappers=1 "$media" > "$output"
    grep -Fxq 'TAG:PARENT/CHILD=VALUE' "$output"
}

run_a1() {
    local media="$case_dir/a1.asf"
    local asf="$case_dir/a1-asf.json"
    local asf_o="$case_dir/a1-asf-o.json"
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i sine=frequency=440:duration=1 -c:a wmav2 -f asf "$media"
    ffprobe -v error -f asf -show_format -show_streams -of json "$media" > "$asf"
    ffprobe -v error -f asf_o -show_format -show_streams -of json "$media" > "$asf_o"
    if cmp -s -- "$asf" "$asf_o"; then
        printf 'FAIL A1: asf and asf_o became indistinguishable\n' >&2
        exit 1
    fi
    grep -Fq '"format_name": "asf"' "$asf"
    grep -Fq '"format_name": "asf_o"' "$asf_o"
    grep -Fq '"encoder": "Lavf' "$asf"
    grep -Fq '"WM/EncodingSettings": "Lavf' "$asf_o"
}

run_n1() {
    local input="$case_dir/n1-input.mkv"
    local matroska="$case_dir/n1-output.mkv"
    local mp4="$case_dir/n1-output.mp4"
    local expected=$'0,0.000000\n1,0.000000\n0,0.040000\n1,0.040000'
    ffmpeg -hide_banner -loglevel error \
        -f lavfi -i testsrc=size=64x48:rate=25:duration=1 \
        -f lavfi -i testsrc2=size=64x48:rate=25:duration=1 \
        -map 0:v -map 1:v -c:v mpeg4 -shortest "$input"
    ffmpeg -hide_banner -loglevel error -i "$input" -map 0 -c copy "$matroska"
    ffmpeg -hide_banner -loglevel error -i "$input" -map 0 -c copy "$mp4"

    # Both tracks have the same DTS at every 40 ms tick. On this tie, stream
    # index 0 is emitted before stream index 1 in either target container.
    assert_equal "$(ffprobe -v error -show_entries packet=stream_index,dts_time \
        -of csv=p=0 "$matroska" | head -n 4)" "$expected" "N1 Matroska tie order"
    assert_equal "$(ffprobe -v error -show_entries packet=stream_index,dts_time \
        -of csv=p=0 "$mp4" | head -n 4)" "$expected" "N1 MP4 tie order"
}

require_reference
case "$case_id" in
    P1) run_p1 ;;
    P2) run_p2 ;;
    P3) run_p3 ;;
    P4) run_p4 ;;
    P5) run_p5 ;;
    P6) run_p6 ;;
    P7) run_p7 ;;
    T1) run_t1 ;;
    T2) run_t2 ;;
    T3) run_t3 ;;
    T4) run_t4 ;;
    T5) run_t5 ;;
    S1) run_s1 ;;
    M1) run_m1 ;;
    M3) run_m3 ;;
    M5) run_m5 ;;
    M7) run_m7 ;;
    K2) run_k2 ;;
    K3) run_k3 ;;
    K4) run_k4 ;;
    A1) run_a1 ;;
    N1) run_n1 ;;
    all)
        run_p1
        run_p2
        run_p3
        run_p4
        run_p5
        run_p6
        run_p7
        run_t1
        run_t2
        run_t3
        run_t4
        run_t5
        run_s1
        run_m1
        run_m3
        run_m5
        run_m7
        run_k2
        run_k3
        run_k4
        run_a1
        run_n1
        ;;
    *)
        printf 'usage: %s [all|P1|P2|P3|P4|P5|P6|P7|T1|T2|T3|T4|T5|S1|M1|M3|M5|M7|K2|K3|K4|A1|N1]\n' "$0" >&2
        exit 2
        ;;
esac

printf 'PASS %s (ffmpeg: %s)\n' "$case_id" "$(ffmpeg -version | head -n 1)"
