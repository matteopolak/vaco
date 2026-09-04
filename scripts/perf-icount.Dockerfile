# The instruction-count measurement environment.
#
# Valgrind has no macOS/Apple-silicon port, and this project is developed on
# one. Docker Desktop on Apple silicon runs arm64 Linux natively (no emulation),
# so cachegrind runs here at normal speed, and the same image runs unchanged on
# a Linux CI runner.
#
# `rust:1-trixie` is the base rather than a slim runtime image so that the vaco
# binary is BUILT and MEASURED against the same glibc. ffmpeg comes from Debian
# and is used strictly as a black box (D6/D7).
FROM rust:1-trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
      valgrind ffmpeg ca-certificates python3 \
 && rm -rf /var/lib/apt/lists/*
