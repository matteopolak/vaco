# `vaco-hw-perf-event` — Linux hardware CPU-cycle counters

## What it is

`vaco-hw-perf-event` is a deliberately tiny Linux OS-binding crate used by
`vaco-checkasm` benchmark mode. It exposes direct, per-thread hardware
`CPU_CYCLES` measurements without making any caller use `unsafe` or handle a
raw file descriptor.

## How it works

On Linux `x86_64` and `aarch64`, `CpuCycles::open_for_current_thread()` opens
one disabled, pinned `PERF_COUNT_HW_CPU_CYCLES` event for `pid = 0, cpu = -1`.
`measure` resets and enables the counter, invokes its closure, then disables
and reads the value. Reset/enable and disable/read are outside the closure.

The request excludes kernel and hypervisor execution. Its read format includes
`time_enabled` and `time_running`; a zero running time or unequal values is a
failure, never a scaled estimate. This is what makes a successful result a
direct hardware-cycle count. Permission failure, unavailable PMU events,
pinned-counter EOF, ioctl failure, partial reads, and multiplexing are all
reported to the caller, which can use an honest `Instant` nanosecond fallback.

The only unsafe code is the narrow Linux UAPI boundary: `perf_event_open` via
`syscall`, three perf ioctls, and `read`, plus transferring a successful
descriptor to `OwnedFd`. The UAPI struct has compile-time and test assertions
for its 128-byte layout and critical offsets.

## How to change it

Keep this crate limited to the single CPU-cycle event. Adding another event or
read-format field requires updating the `perf_event_attr`/read structs,
layout assertions, error behavior, and this document together. Do not scale a
multiplexed counter: callers rely on failure to prevent estimated values from
being reported as cycles.

## Configuration

There are no crate-level flags. Linux controls access through PMU availability,
capabilities, and `perf_event_paranoid`. Off Linux `x86_64`/`aarch64`, opening
returns `CounterError::UnsupportedTarget`.

## Dependencies

No third-party dependency is used. The implementation calls the host Linux
kernel UAPI described by `linux-perf-event-open(2)` in
`provenance/sources.toml`.
