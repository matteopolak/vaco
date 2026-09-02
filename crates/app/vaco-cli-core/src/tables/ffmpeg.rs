/// The `vaco` (ffmpeg-equivalent) argv-flag table, as a `CliOptionTable`
/// derive input rather than a hand-written `&[OptDesc]` literal.
///
/// Names, argument placeholders, scopes and specifier kinds are interface
/// facts about ffmpeg 8.1 (D9 permits reproducing those). The help strings
/// are NOT the reference's; they were written for Vaco, because D9 forbids
/// reproducing help text. `-h` output therefore cannot be byte-identical,
/// by design.
///
/// An alias variant (`alias_of = "..."`) inherits whatever it does not
/// override from its target -- see the derive's own doc for why, and for
/// the one case (`B`, aliasing itself with `spec = "v"`) where a variant
/// names itself: that is how a bare `-b` means `-b:v`, per
/// `ParsedOption::resolved`.
// Never constructed as a value -- purely a `CliOptionTable` derive input,
// read for its variant attributes at compile time. The real runtime data
// is `OPTIONS`, generated below.
#[allow(dead_code)]
#[derive(vaco_opts::CliOptionTable)]
pub(crate) enum FfmpegOptions {
    #[cli(name = "L", flags(EXIT, GLOBAL), kind = None, help = "print the licence")]
    L,
    #[cli(name = "license", alias_of = "L")]
    License,
    #[cli(name = "h", argname = "topic", flags(EXIT, GLOBAL, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "print help on a topic")]
    H,
    #[cli(name = "version", flags(EXIT, GLOBAL), kind = None, help = "print the version")]
    Version,
    #[cli(name = "muxers", flags(EXIT, GLOBAL), kind = None, help = "list muxers")]
    Muxers,
    #[cli(name = "demuxers", flags(EXIT, GLOBAL), kind = None, help = "list demuxers")]
    Demuxers,
    #[cli(name = "devices", flags(EXIT, GLOBAL), kind = None, help = "list devices")]
    Devices,
    #[cli(name = "decoders", flags(EXIT, GLOBAL), kind = None, help = "list decoders")]
    Decoders,
    #[cli(name = "encoders", flags(EXIT, GLOBAL), kind = None, help = "list encoders")]
    Encoders,
    #[cli(name = "filters", flags(EXIT, GLOBAL), kind = None, help = "list filters")]
    Filters,
    #[cli(name = "pix_fmts", flags(EXIT, GLOBAL), kind = None, help = "list pixel formats")]
    PixFmts,
    #[cli(name = "layouts", flags(EXIT, GLOBAL), kind = None, help = "list channel layouts")]
    Layouts,
    #[cli(name = "sample_fmts", flags(EXIT, GLOBAL), kind = None, help = "list sample formats")]
    SampleFmts,
    #[cli(name = "?", alias_of = "h", flags(EXIT, GLOBAL, EXPERT, HAS_ARG, OPTIONAL_ARG))]
    Question,
    #[cli(name = "help", alias_of = "h", flags(EXIT, GLOBAL, EXPERT, HAS_ARG, OPTIONAL_ARG))]
    Help,
    #[cli(name = "-help", alias_of = "h", flags(EXIT, GLOBAL, EXPERT, HAS_ARG, OPTIONAL_ARG))]
    DashHelp,
    #[cli(name = "buildconf", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "print the build configuration")]
    Buildconf,
    #[cli(name = "formats", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list container formats")]
    Formats,
    #[cli(name = "codecs", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list codecs")]
    Codecs,
    #[cli(name = "bsfs", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list bitstream filters")]
    Bsfs,
    #[cli(name = "protocols", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list protocols")]
    Protocols,
    #[cli(name = "dispositions", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list stream dispositions")]
    Dispositions,
    #[cli(name = "colors", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list named colours")]
    Colors,
    #[cli(name = "sources", argname = "device", flags(EXIT, GLOBAL, EXPERT, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "list a device's input sources")]
    Sources,
    #[cli(name = "sinks", argname = "device", flags(EXIT, GLOBAL, EXPERT, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "list a device's output sinks")]
    Sinks,
    #[cli(name = "hwaccels", flags(EXIT, GLOBAL, EXPERT), kind = None, help = "list hardware acceleration methods")]
    Hwaccels,
    #[cli(name = "v", alias_of = "loglevel", flags(HAS_ARG, GLOBAL))]
    V,
    #[cli(name = "y", flags(GLOBAL), kind = None, help = "overwrite output files without asking")]
    Y,
    #[cli(name = "n", flags(GLOBAL), kind = None, help = "refuse to overwrite output files")]
    N,
    #[cli(name = "print_graphs", flags(GLOBAL), kind = None, help = "dump the execution graph to stderr")]
    PrintGraphs,
    #[cli(name = "print_graphs_file", argname = "filename", flags(HAS_ARG, GLOBAL), kind = Str, help = "write the execution graph to a file")]
    PrintGraphsFile,
    #[cli(name = "print_graphs_format", argname = "format", flags(HAS_ARG, GLOBAL), kind = Str, help = "choose the execution graph's writer")]
    PrintGraphsFormat,
    #[cli(name = "stats", flags(GLOBAL), kind = None, help = "print a progress report while running")]
    Stats,
    #[cli(name = "loglevel", argname = "loglevel", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "set the log level")]
    Loglevel,
    #[cli(name = "report", flags(GLOBAL, EXPERT), kind = None, help = "write a debug log file for this run")]
    Report,
    #[cli(name = "max_alloc", argname = "bytes", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "cap a single allocation")]
    MaxAlloc,
    #[cli(name = "cpuflags", argname = "flags", flags(HAS_ARG, GLOBAL, EXPERT), kind = Expr, help = "override detected CPU features")]
    Cpuflags,
    #[cli(name = "cpucount", argname = "count", flags(HAS_ARG, GLOBAL, EXPERT), kind = Expr, help = "override the detected CPU count")]
    Cpucount,
    #[cli(name = "hide_banner", argname = "hide_banner", flags(GLOBAL, EXPERT), kind = None, help = "suppress the version banner")]
    HideBanner,
    #[cli(name = "ignore_unknown", flags(GLOBAL, EXPERT), kind = None, help = "drop streams of unknown type")]
    IgnoreUnknown,
    #[cli(name = "copy_unknown", flags(GLOBAL, EXPERT), kind = None, help = "copy streams of unknown type")]
    CopyUnknown,
    #[cli(name = "recast_media", flags(GLOBAL, EXPERT), kind = None, help = "allow forcing a decoder of another media type")]
    RecastMedia,
    #[cli(name = "benchmark", flags(GLOBAL, EXPERT), kind = None, help = "report timing for the whole run")]
    Benchmark,
    #[cli(name = "benchmark_all", flags(GLOBAL, EXPERT), kind = None, help = "report timing for every task")]
    BenchmarkAll,
    #[cli(name = "progress", argname = "url", flags(HAS_ARG, GLOBAL, EXPERT), kind = Str, help = "write machine-readable progress to a URL")]
    Progress,
    #[cli(name = "stdin", flags(GLOBAL, EXPERT), kind = None, help = "enable or disable standard-input interaction")]
    Stdin,
    #[cli(name = "timelimit", argname = "limit", flags(HAS_ARG, GLOBAL, EXPERT), kind = Int, help = "abort after this much CPU time")]
    Timelimit,
    #[cli(name = "dump", flags(GLOBAL, EXPERT), kind = None, help = "dump every input packet")]
    Dump,
    #[cli(name = "hex", flags(GLOBAL, EXPERT), kind = None, help = "include payloads in packet dumps")]
    Hex,
    #[cli(name = "frame_drop_threshold", flags(HAS_ARG, GLOBAL, EXPERT), kind = Float, help = "threshold for dropping a late frame")]
    FrameDropThreshold,
    #[cli(name = "copyts", flags(GLOBAL, EXPERT), kind = None, help = "do not rebase input timestamps")]
    Copyts,
    #[cli(name = "start_at_zero", flags(GLOBAL, EXPERT), kind = None, help = "shift copied timestamps to start at zero")]
    StartAtZero,
    #[cli(name = "copytb", argname = "mode", flags(HAS_ARG, GLOBAL, EXPERT), kind = Int, help = "choose the time base when stream copying")]
    Copytb,
    #[cli(name = "dts_delta_threshold", argname = "threshold", flags(HAS_ARG, GLOBAL, EXPERT), kind = Float, help = "discontinuity threshold on decode timestamps")]
    DtsDeltaThreshold,
    #[cli(name = "dts_error_threshold", argname = "threshold", flags(HAS_ARG, GLOBAL, EXPERT), kind = Float, help = "error threshold on decode timestamps")]
    DtsErrorThreshold,
    #[cli(name = "xerror", argname = "error", flags(GLOBAL, EXPERT), kind = None, help = "exit on the first error")]
    Xerror,
    #[cli(name = "abort_on", argname = "flags", flags(HAS_ARG, GLOBAL, EXPERT), kind = Expr, help = "conditions that abort the run")]
    AbortOn,
    /// An `AVCodecContext` option in the reference, not an entry in
    /// `ffmpeg.c`'s own table, so it does not appear in `-h full` and was
    /// never extracted from it. Vaco has no per-codec option store yet, so
    /// this is global: `-threads N` before or after `-i` both mean "N
    /// threads for every codec in this run". Per-stream (`-threads:v:0`)
    /// is not implemented. Default `min(available_parallelism, 4)`, not
    /// the reference's "auto" (`0`) -- see `crate::cli::default_thread_count`.
    #[cli(name = "threads", argname = "count", flags(HAS_ARG, GLOBAL), kind = Int, help = "decoding threads per codec (default: min(cores, 4); 1 = single-threaded)")]
    Threads,
    #[cli(name = "filter_threads", flags(HAS_ARG, GLOBAL, EXPERT), kind = Str, help = "threads for simple filter graphs")]
    FilterThreads,
    #[cli(name = "filter_buffered_frames", flags(HAS_ARG, GLOBAL, EXPERT), kind = Int, help = "frames a filter graph may buffer")]
    FilterBufferedFrames,
    #[cli(name = "filter_complex", argname = "graph_description", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "define a complex filter graph")]
    FilterComplex,
    #[cli(name = "filter_complex_threads", flags(HAS_ARG, GLOBAL, EXPERT), kind = Int, help = "threads for complex filter graphs")]
    FilterComplexThreads,
    #[cli(name = "lavfi", alias_of = "filter_complex")]
    Lavfi,
    #[cli(name = "filter_complex_script", argname = "filename", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "read a complex filter graph from a file")]
    FilterComplexScript,
    #[cli(name = "auto_conversion_filters", flags(GLOBAL, EXPERT), kind = None, help = "insert format conversion filters automatically")]
    AutoConversionFilters,
    #[cli(name = "stats_period", argname = "time", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "how often to refresh progress output")]
    StatsPeriod,
    #[cli(name = "debug_ts", flags(GLOBAL, EXPERT), kind = None, help = "trace timestamps")]
    DebugTs,
    #[cli(name = "max_error_rate", argname = "maximum error rate", flags(HAS_ARG, GLOBAL, EXPERT), kind = Float, help = "decoding error ratio that fails the run")]
    MaxErrorRate,
    #[cli(name = "vstats", flags(GLOBAL, EXPERT), kind = None, help = "write video coding statistics")]
    Vstats,
    #[cli(name = "vstats_file", argname = "file", flags(HAS_ARG, GLOBAL, EXPERT), kind = Str, help = "file for video coding statistics")]
    VstatsFile,
    #[cli(name = "vstats_version", flags(HAS_ARG, GLOBAL, EXPERT), kind = Int, help = "video statistics format version")]
    VstatsVersion,
    #[cli(name = "sdp_file", argname = "file", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "write the SDP description to a file")]
    SdpFile,
    #[cli(name = "init_hw_device", argname = "args", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "create a hardware device")]
    InitHwDevice,
    #[cli(name = "filter_hw_device", argname = "device", flags(HAS_ARG, GLOBAL, EXPERT), kind = Custom, help = "hardware device for filtering")]
    FilterHwDevice,
    #[cli(name = "adrift_threshold", argname = "threshold", flags(HAS_ARG, GLOBAL, EXPERT), kind = Str, help = "deprecated, has no effect")]
    AdriftThreshold,
    #[cli(name = "qphist", flags(GLOBAL, EXPERT), kind = None, help = "deprecated, has no effect")]
    Qphist,
    #[cli(name = "vsync", flags(HAS_ARG, GLOBAL, EXPERT), kind = Float, help = "deprecated, use fps_mode")]
    Vsync,
    #[cli(name = "f", argname = "fmt", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT), kind = Str, help = "force the container format")]
    F,
    #[cli(name = "t", argname = "duration", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT), kind = Duration, help = "stop after this duration")]
    T,
    #[cli(name = "to", argname = "time_stop", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT), kind = Duration, help = "stop at this position")]
    To,
    #[cli(name = "ss", argname = "time_off", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT), kind = Duration, help = "start at this position")]
    Ss,
    #[cli(name = "bitexact", flags(PER_FILE, INPUT, OUTPUT, EXPERT), kind = None, help = "restrict output to bit-exact operations")]
    Bitexact,
    #[cli(name = "thread_queue_size", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, EXPERT), kind = Int, help = "packets the demuxer may queue")]
    ThreadQueueSize,
    #[cli(name = "sseof", argname = "time_off", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Duration, help = "start at this position relative to the end")]
    Sseof,
    #[cli(name = "seek_timestamp", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Int, help = "seek by timestamp rather than by position")]
    SeekTimestamp,
    #[cli(name = "accurate_seek", flags(PER_FILE, INPUT, EXPERT), kind = None, help = "decode up to the exact seek position")]
    AccurateSeek,
    #[cli(name = "isync", argname = "sync ref", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Int, help = "input whose clock this input follows")]
    Isync,
    #[cli(name = "itsoffset", argname = "time_off", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Duration, help = "shift this input's timestamps")]
    Itsoffset,
    #[cli(name = "re", flags(PER_FILE, INPUT, EXPERT), kind = None, help = "read the input at its native rate")]
    Re,
    #[cli(name = "readrate", argname = "speed", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Float, help = "read the input at this multiple of native rate")]
    Readrate,
    #[cli(name = "readrate_initial_burst", argname = "seconds", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Float, help = "read this much before rate limiting starts")]
    ReadrateInitialBurst,
    #[cli(name = "readrate_catchup", argname = "speed", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Float, help = "rate used to catch up after falling behind")]
    ReadrateCatchup,
    #[cli(name = "dump_attachment", argname = "filename", flags(HAS_ARG, PER_FILE, INPUT, TAKES_SPEC, EXPERT), kind = Str, help = "write an attachment to a file")]
    DumpAttachment,
    #[cli(name = "stream_loop", argname = "loop count", flags(HAS_ARG, PER_FILE, INPUT, EXPERT), kind = Int, help = "repeat the input this many times")]
    StreamLoop,
    #[cli(name = "find_stream_info", flags(PER_FILE, INPUT, EXPERT), kind = None, help = "probe the input before opening it")]
    FindStreamInfo,
    #[cli(name = "metadata", argname = "key=value", flags(HAS_ARG, PER_FILE, OUTPUT, TAKES_SPEC), kind = Custom, help = "set a metadata entry")]
    Metadata,
    #[cli(name = "map", argname = "[-]input_file_id[:stream_specifier][,sync_file_id[:stream_specifier]]", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Custom, help = "select an input stream for the output")]
    Map,
    #[cli(name = "map_metadata", argname = "outfile[,metadata]:infile[,metadata]", flags(HAS_ARG, PER_FILE, OUTPUT, TAKES_SPEC, EXPERT), kind = Custom, help = "copy metadata from an input")]
    MapMetadata,
    #[cli(name = "map_chapters", argname = "input_file_index", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Int, help = "copy chapters from an input")]
    MapChapters,
    #[cli(name = "fs", argname = "limit_size", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Int64, help = "stop once the output reaches this size")]
    Fs,
    #[cli(name = "timestamp", argname = "time", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Custom, help = "set the recording timestamp")]
    Timestamp,
    #[cli(name = "program", argname = "title=string:st=number...", flags(HAS_ARG, PER_FILE, OUTPUT, TAKES_SPEC, EXPERT), kind = Custom, help = "create a program from the given streams")]
    Program,
    #[cli(name = "stream_group", argname = "id=number:st=number...", flags(HAS_ARG, PER_FILE, OUTPUT, TAKES_SPEC, EXPERT), kind = Custom, help = "create a stream group from the given streams")]
    StreamGroup,
    #[cli(name = "dframes", alias_of = "frames", spec = "d", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), help = "stop after this many data frames")]
    Dframes,
    #[cli(name = "target", argname = "type", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Custom, help = "apply a preset for a standard target")]
    Target,
    #[cli(name = "shortest", flags(PER_FILE, OUTPUT, EXPERT), kind = None, help = "stop when the shortest input ends")]
    Shortest,
    #[cli(name = "shortest_buf_duration", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Float, help = "buffering allowed while waiting for the shortest input")]
    ShortestBufDuration,
    #[cli(name = "qscale", alias_of = "q", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT))]
    Qscale,
    #[cli(name = "profile", argname = "profile", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Expr, help = "select the encoder profile")]
    Profile,
    #[cli(name = "attach", argname = "filename", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Str, help = "attach a file to the output")]
    Attach,
    #[cli(name = "muxdelay", argname = "seconds", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Float, help = "maximum demux-to-decode delay")]
    Muxdelay,
    #[cli(name = "muxpreload", argname = "seconds", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Float, help = "initial demux-to-decode delay")]
    Muxpreload,
    #[cli(name = "fpre", argname = "filename", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT), kind = Custom, help = "load options from a preset file")]
    Fpre,
    #[cli(name = "c", argname = "codec", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM), kind = Str, help = "select the codec, or copy to remux")]
    C,
    #[cli(name = "filter", argname = "filter_graph", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM), kind = Custom, help = "apply a filter graph")]
    Filter,
    #[cli(name = "codec", alias_of = "c", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT))]
    Codec,
    #[cli(name = "pre", argname = "preset", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "load a named preset")]
    Pre,
    #[cli(name = "itsscale", argname = "scale", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT), kind = Float, help = "scale this stream's timestamps")]
    Itsscale,
    #[cli(name = "copyinkf", flags(PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = None, help = "copy leading non-keyframes")]
    Copyinkf,
    #[cli(name = "copypriorss", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Int, help = "keep frames that precede the start time")]
    Copypriorss,
    #[cli(name = "frames", argname = "number", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Int64, help = "stop after this many frames")]
    Frames,
    #[cli(name = "tag", argname = "fourcc/tag", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "force the codec tag")]
    Tag,
    #[cli(name = "q", argname = "q", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Float, help = "use a fixed quality scale")]
    Q,
    #[cli(name = "filter_script", argname = "filename", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Custom, help = "read a filter graph from a file")]
    FilterScript,
    #[cli(name = "reinit_filter", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT), kind = Int, help = "rebuild the filter graph when input parameters change")]
    ReinitFilter,
    #[cli(name = "drop_changed", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT), kind = Int, help = "drop frames instead of rebuilding the filter graph")]
    DropChanged,
    #[cli(name = "discard", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT), kind = Expr, help = "discard packets matching a condition")]
    Discard,
    #[cli(name = "disposition", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Expr, help = "set the output stream's disposition")]
    Disposition,
    #[cli(name = "bits_per_raw_sample", argname = "number", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Int, help = "declare the sample depth")]
    BitsPerRawSample,
    #[cli(name = "stats_enc_pre", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "write encoder statistics before encoding")]
    StatsEncPre,
    #[cli(name = "stats_enc_post", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "write encoder statistics after encoding")]
    StatsEncPost,
    #[cli(name = "stats_mux_pre", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "write muxer statistics before muxing")]
    StatsMuxPre,
    #[cli(name = "stats_enc_pre_fmt", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "format of the pre-encode statistics")]
    StatsEncPreFmt,
    #[cli(name = "stats_enc_post_fmt", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "format of the post-encode statistics")]
    StatsEncPostFmt,
    #[cli(name = "stats_mux_pre_fmt", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "format of the pre-mux statistics")]
    StatsMuxPreFmt,
    #[cli(name = "time_base", argname = "ratio", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Expr, help = "suggest the output stream's time base")]
    TimeBase,
    #[cli(name = "enc_time_base", argname = "ratio", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Str, help = "set the encoder's time base")]
    EncTimeBase,
    #[cli(name = "bsf", argname = "bitstream_filters", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT), kind = Custom, help = "bitstream filters to apply")]
    Bsf,
    #[cli(name = "max_muxing_queue_size", argname = "packets", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Int, help = "packets buffered while streams initialise")]
    MaxMuxingQueueSize,
    #[cli(name = "muxing_queue_data_threshold", argname = "bytes", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT), kind = Int, help = "bytes buffered before the queue limit applies")]
    MuxingQueueDataThreshold,
    #[cli(name = "r", argname = "rate", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, VIDEO), kind = Rate, help = "set the frame rate")]
    R,
    #[cli(name = "s", argname = "size", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, VIDEO), kind = Size, help = "set frame size")]
    S,
    #[cli(name = "aspect", argname = "aspect", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, VIDEO), kind = Expr, help = "set the display aspect ratio")]
    Aspect,
    #[cli(name = "vn", flags(PER_FILE, INPUT, OUTPUT, VIDEO), kind = None, help = "drop video streams")]
    Vn,
    #[cli(name = "vcodec", alias_of = "c", spec = "v", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, VIDEO), help = "select the video codec")]
    Vcodec,
    #[cli(name = "vf", alias_of = "filter", spec = "v", flags(HAS_ARG, PER_FILE, OUTPUT, VIDEO), help = "apply a video filter graph")]
    Vf,
    #[cli(name = "b", alias_of = "b", spec = "v", argname = "bitrate", flags(HAS_ARG, PER_FILE, OUTPUT, VIDEO), kind = Expr, help = "set the video bitrate")]
    B,
    #[cli(name = "vframes", alias_of = "frames", spec = "v", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, VIDEO), help = "stop after this many video frames")]
    Vframes,
    #[cli(name = "fpsmax", argname = "rate", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Custom, help = "cap the output frame rate")]
    Fpsmax,
    #[cli(name = "pix_fmt", argname = "format", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "set the pixel format")]
    PixFmt,
    #[cli(name = "display_rotation", argname = "angle", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = Float, help = "set the display rotation")]
    DisplayRotation,
    #[cli(name = "display_hflip", flags(PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "flip the display horizontally")]
    DisplayHflip,
    #[cli(name = "display_vflip", flags(PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "flip the display vertically")]
    DisplayVflip,
    #[cli(name = "rc_override", argname = "override", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "rate control override for an interval")]
    RcOverride,
    #[cli(name = "timecode", argname = "hh:mm:ss[:;.]ff", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, VIDEO), kind = Str, help = "set the starting timecode")]
    Timecode,
    #[cli(name = "pass", argname = "n", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Int, help = "select the encoding pass")]
    Pass,
    #[cli(name = "passlogfile", argname = "prefix", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "prefix for the two-pass log")]
    Passlogfile,
    #[cli(name = "intra_matrix", argname = "matrix", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "intra quantiser matrix")]
    IntraMatrix,
    #[cli(name = "inter_matrix", argname = "matrix", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "inter quantiser matrix")]
    InterMatrix,
    #[cli(name = "chroma_intra_matrix", argname = "matrix", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "chroma intra quantiser matrix")]
    ChromaIntraMatrix,
    #[cli(name = "vtag", alias_of = "tag", spec = "v", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, EXPERT, VIDEO), help = "force the video codec tag")]
    Vtag,
    #[cli(name = "fps_mode", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "how frame rate is reconciled")]
    FpsMode,
    #[cli(name = "force_fps", flags(PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "do not negotiate the frame rate")]
    ForceFps,
    #[cli(name = "streamid", argname = "streamIndex:value", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, VIDEO), kind = Custom, help = "set an output stream's id")]
    Streamid,
    #[cli(name = "force_key_frames", argname = "timestamps", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "force key frames at these positions")]
    ForceKeyFrames,
    #[cli(name = "hwaccel", argname = "hwaccel name", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = Custom, help = "use hardware-accelerated decoding")]
    Hwaccel,
    #[cli(name = "hwaccel_device", argname = "devicename", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "device for hardware decoding")]
    HwaccelDevice,
    #[cli(name = "hwaccel_output_format", argname = "format", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = Str, help = "pixel format produced by hardware decoding")]
    HwaccelOutputFormat,
    #[cli(name = "autorotate", flags(PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "apply the input's rotation metadata")]
    Autorotate,
    #[cli(name = "autoscale", flags(PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "scale automatically at the end of the filter graph")]
    Autoscale,
    #[cli(name = "apply_cropping", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, VIDEO), kind = Expr, help = "apply the input's cropping metadata")]
    ApplyCropping,
    #[cli(name = "fix_sub_duration_heartbeat", flags(PER_FILE, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = None, help = "use this stream to split open subtitles")]
    FixSubDurationHeartbeat,
    #[cli(name = "vpre", alias_of = "pre", spec = "v", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, VIDEO), kind = Custom, help = "load a video preset")]
    Vpre,
    #[cli(name = "top", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT, VIDEO), kind = Int, help = "deprecated, use the setfield filter")]
    Top,
    #[cli(name = "aq", alias_of = "q", spec = "a", argname = "quality", flags(HAS_ARG, PER_FILE, OUTPUT, AUDIO), help = "set the audio quality")]
    Aq,
    #[cli(name = "ar", argname = "rate", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, AUDIO), kind = Int, help = "set the sample rate")]
    Ar,
    #[cli(name = "ac", argname = "channels", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, AUDIO), kind = Int, help = "set the channel count")]
    Ac,
    #[cli(name = "an", flags(PER_FILE, INPUT, OUTPUT, AUDIO), kind = None, help = "drop audio streams")]
    An,
    #[cli(name = "acodec", alias_of = "c", spec = "a", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, AUDIO), help = "select the audio codec")]
    Acodec,
    #[cli(name = "ab", alias_of = "b", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, AUDIO), help = "set the audio bitrate")]
    Ab,
    #[cli(name = "af", alias_of = "filter", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, AUDIO), help = "apply an audio filter graph")]
    Af,
    #[cli(name = "aframes", alias_of = "frames", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, AUDIO), help = "stop after this many audio frames")]
    Aframes,
    #[cli(name = "apad", flags(HAS_ARG, PER_FILE, OUTPUT, PER_STREAM, EXPERT, AUDIO), kind = Str, help = "pad the audio with silence")]
    Apad,
    #[cli(name = "atag", alias_of = "tag", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, AUDIO), help = "force the audio codec tag")]
    Atag,
    #[cli(name = "sample_fmt", argname = "format", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT, AUDIO), kind = Str, help = "set the sample format")]
    SampleFmt,
    #[cli(name = "channel_layout", argname = "layout", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT, AUDIO), kind = Str, help = "set the channel layout")]
    ChannelLayout,
    #[cli(name = "ch_layout", argname = "layout", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, PER_STREAM, EXPERT, AUDIO), kind = Str, help = "set the channel layout")]
    ChLayout,
    #[cli(name = "guess_layout_max", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, AUDIO), kind = Int, help = "channel count up to which the layout is guessed")]
    GuessLayoutMax,
    #[cli(name = "apre", alias_of = "pre", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, AUDIO), kind = Custom, help = "load an audio preset")]
    Apre,
    #[cli(name = "sn", flags(PER_FILE, INPUT, OUTPUT, SUBTITLE), kind = None, help = "drop subtitle streams")]
    Sn,
    #[cli(name = "scodec", alias_of = "c", spec = "s", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, SUBTITLE), help = "select the subtitle codec")]
    Scodec,
    #[cli(name = "stag", alias_of = "tag", spec = "s", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, SUBTITLE), help = "force the subtitle codec tag")]
    Stag,
    #[cli(name = "fix_sub_duration", flags(PER_FILE, INPUT, PER_STREAM, EXPERT, SUBTITLE), kind = None, help = "derive subtitle durations from the next event")]
    FixSubDuration,
    #[cli(name = "canvas_size", argname = "size", flags(HAS_ARG, PER_FILE, INPUT, PER_STREAM, EXPERT, SUBTITLE), kind = Str, help = "set the subtitle canvas size")]
    CanvasSize,
    #[cli(name = "spre", alias_of = "pre", spec = "s", flags(HAS_ARG, PER_FILE, OUTPUT, EXPERT, SUBTITLE), kind = Custom, help = "load a subtitle preset")]
    Spre,
    #[cli(name = "dcodec", alias_of = "c", spec = "d", flags(HAS_ARG, PER_FILE, INPUT, OUTPUT, EXPERT, DATA), help = "select the data codec")]
    Dcodec,
    #[cli(name = "dn", flags(PER_FILE, INPUT, OUTPUT, EXPERT, DATA), kind = None, help = "drop data streams")]
    Dn,
    /// Does not appear in `ffmpeg -h full` at all -- hidden from the
    /// grouped help -- so it is declared here by hand rather than lifted
    /// from an extraction.
    #[cli(name = "i", argname = "input_file", flags(HAS_ARG, PER_FILE, INPUT, OPENS_INPUT), kind = Str, help = "read from this input URL")]
    I,
}

pub(crate) static FFMPEG_OPTIONS: &[OptDesc] = FfmpegOptions::OPTIONS;
