/// The `vaco-probe` (ffprobe-equivalent) argv-flag table.
///
/// ffprobe presents one flat "Main options" section, so scope is derived
/// differently from `ffmpeg.rs`: everything that prints and exits is
/// EXIT|GLOBAL, `-i` opens the input, and the remainder is GLOBAL --
/// ffprobe has no per-file option groups at all, which is why its
/// `-select_streams` is a single global value rather than a per-stream one.
// Never constructed as a value -- purely a `CliOptionTable` derive input,
// read for its variant attributes at compile time. The real runtime data
// is `OPTIONS`, generated below.
#[allow(dead_code)]
#[derive(vaco_opts::CliOptionTable)]
pub(crate) enum FfprobeOptions {
    #[cli(name = "L", flags(EXIT, GLOBAL), kind = None, help = "print the licence")]
    L,
    #[cli(name = "license", alias_of = "L")]
    License,
    #[cli(name = "h", argname = "topic", flags(EXIT, GLOBAL, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "print help on a topic")]
    H,
    #[cli(name = "?", alias_of = "h")]
    Question,
    #[cli(name = "help", alias_of = "h")]
    Help,
    #[cli(name = "-help", alias_of = "h")]
    DashHelp,
    #[cli(name = "version", flags(EXIT, GLOBAL), kind = None, help = "print the version")]
    Version,
    #[cli(name = "buildconf", flags(EXIT, GLOBAL), kind = None, help = "print the build configuration")]
    Buildconf,
    #[cli(name = "formats", flags(EXIT, GLOBAL), kind = None, help = "list container formats")]
    Formats,
    #[cli(name = "muxers", flags(EXIT, GLOBAL), kind = None, help = "list muxers")]
    Muxers,
    #[cli(name = "demuxers", flags(EXIT, GLOBAL), kind = None, help = "list demuxers")]
    Demuxers,
    #[cli(name = "devices", flags(EXIT, GLOBAL), kind = None, help = "list devices")]
    Devices,
    #[cli(name = "codecs", flags(EXIT, GLOBAL), kind = None, help = "list codecs")]
    Codecs,
    #[cli(name = "decoders", flags(EXIT, GLOBAL), kind = None, help = "list decoders")]
    Decoders,
    #[cli(name = "encoders", flags(EXIT, GLOBAL), kind = None, help = "list encoders")]
    Encoders,
    #[cli(name = "bsfs", flags(EXIT, GLOBAL), kind = None, help = "list bitstream filters")]
    Bsfs,
    #[cli(name = "protocols", flags(EXIT, GLOBAL), kind = None, help = "list protocols")]
    Protocols,
    #[cli(name = "filters", flags(EXIT, GLOBAL), kind = None, help = "list filters")]
    Filters,
    #[cli(name = "pix_fmts", flags(EXIT, GLOBAL), kind = None, help = "list pixel formats")]
    PixFmts,
    #[cli(name = "layouts", flags(EXIT, GLOBAL), kind = None, help = "list channel layouts")]
    Layouts,
    #[cli(name = "sample_fmts", flags(EXIT, GLOBAL), kind = None, help = "list sample formats")]
    SampleFmts,
    #[cli(name = "dispositions", flags(EXIT, GLOBAL), kind = None, help = "list stream dispositions")]
    Dispositions,
    #[cli(name = "colors", flags(EXIT, GLOBAL), kind = None, help = "list named colours")]
    Colors,
    #[cli(name = "loglevel", argname = "loglevel", flags(HAS_ARG, GLOBAL), kind = Custom, help = "set the log level")]
    Loglevel,
    #[cli(name = "v", alias_of = "loglevel")]
    V,
    #[cli(name = "report", flags(GLOBAL), kind = None, help = "write a debug log file for this run")]
    Report,
    #[cli(name = "max_alloc", argname = "bytes", flags(HAS_ARG, GLOBAL), kind = Custom, help = "cap a single allocation")]
    MaxAlloc,
    #[cli(name = "cpuflags", argname = "flags", flags(HAS_ARG, GLOBAL), kind = Expr, help = "override detected CPU features")]
    Cpuflags,
    #[cli(name = "cpucount", argname = "count", flags(HAS_ARG, GLOBAL), kind = Expr, help = "override the detected CPU count")]
    Cpucount,
    #[cli(name = "hide_banner", argname = "hide_banner", flags(GLOBAL), kind = None, help = "suppress the version banner")]
    HideBanner,
    #[cli(name = "sources", argname = "device", flags(EXIT, GLOBAL, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "list a device's input sources")]
    Sources,
    #[cli(name = "sinks", argname = "device", flags(EXIT, GLOBAL, HAS_ARG, OPTIONAL_ARG), kind = Custom, help = "list a device's output sinks")]
    Sinks,
    #[cli(name = "f", argname = "format", flags(HAS_ARG, GLOBAL), kind = Str, help = "force the container format")]
    F,
    #[cli(name = "unit", flags(GLOBAL), kind = None, help = "print units alongside values")]
    Unit,
    #[cli(name = "prefix", flags(GLOBAL), kind = None, help = "use SI prefixes on values")]
    Prefix,
    #[cli(name = "byte_binary_prefix", flags(GLOBAL), kind = None, help = "use binary prefixes for byte counts")]
    ByteBinaryPrefix,
    #[cli(name = "sexagesimal", flags(GLOBAL), kind = None, help = "print times as HH:MM:SS.mmm")]
    Sexagesimal,
    #[cli(name = "pretty", flags(GLOBAL), kind = None, help = "format values for reading")]
    Pretty,
    #[cli(name = "output_format", argname = "format", flags(HAS_ARG, GLOBAL), kind = Str, help = "select the output writer")]
    OutputFormat,
    #[cli(name = "print_format", alias_of = "output_format")]
    PrintFormat,
    #[cli(name = "of", alias_of = "output_format")]
    Of,
    #[cli(name = "select_streams", argname = "stream_specifier", flags(HAS_ARG, GLOBAL), kind = Custom, help = "restrict output to matching streams")]
    SelectStreams,
    #[cli(name = "sections", flags(EXIT, GLOBAL), kind = None, help = "list the output sections")]
    Sections,
    #[cli(name = "data_dump_format", flags(HAS_ARG, GLOBAL), kind = Str, help = "format for dumped payloads")]
    DataDumpFormat,
    #[cli(name = "show_data", flags(GLOBAL), kind = None, help = "dump packet and frame payloads")]
    ShowData,
    #[cli(name = "show_data_hash", flags(HAS_ARG, GLOBAL), kind = Str, help = "hash packet and frame payloads")]
    ShowDataHash,
    #[cli(name = "show_error", flags(GLOBAL), kind = None, help = "print the error section")]
    ShowError,
    #[cli(name = "show_format", flags(GLOBAL), kind = None, help = "print the container section")]
    ShowFormat,
    #[cli(name = "show_frames", flags(GLOBAL), kind = None, help = "print one section per frame")]
    ShowFrames,
    #[cli(name = "show_entries", argname = "entry_list", flags(HAS_ARG, GLOBAL), kind = Custom, help = "restrict output to these sections and fields")]
    ShowEntries,
    #[cli(name = "show_log", flags(HAS_ARG, GLOBAL), kind = Int, help = "print the decoder log")]
    ShowLog,
    #[cli(name = "show_packets", flags(GLOBAL), kind = None, help = "print one section per packet")]
    ShowPackets,
    #[cli(name = "show_programs", flags(GLOBAL), kind = None, help = "print the program sections")]
    ShowPrograms,
    #[cli(name = "show_stream_groups", flags(GLOBAL), kind = None, help = "print the stream group sections")]
    ShowStreamGroups,
    #[cli(name = "show_streams", flags(GLOBAL), kind = None, help = "print the stream sections")]
    ShowStreams,
    #[cli(name = "show_chapters", flags(GLOBAL), kind = None, help = "print the chapter sections")]
    ShowChapters,
    #[cli(name = "count_frames", flags(GLOBAL), kind = None, help = "count frames per stream")]
    CountFrames,
    #[cli(name = "count_packets", flags(GLOBAL), kind = None, help = "count packets per stream")]
    CountPackets,
    #[cli(name = "show_program_version", flags(GLOBAL), kind = None, help = "print this program's version section")]
    ShowProgramVersion,
    #[cli(name = "show_library_versions", flags(GLOBAL), kind = None, help = "print the library version sections")]
    ShowLibraryVersions,
    #[cli(name = "show_versions", flags(GLOBAL), kind = None, help = "print every version section")]
    ShowVersions,
    #[cli(name = "show_pixel_formats", flags(GLOBAL), kind = None, help = "print the pixel format sections")]
    ShowPixelFormats,
    #[cli(name = "show_optional_fields", flags(HAS_ARG, GLOBAL), kind = Str, help = "when to print fields that may be absent")]
    ShowOptionalFields,
    #[cli(name = "show_private_data", flags(GLOBAL), kind = None, help = "print codec-private fields")]
    ShowPrivateData,
    #[cli(name = "private", alias_of = "show_private_data")]
    Private,
    #[cli(name = "analyze_frames", flags(GLOBAL), kind = None, help = "decode frames to fill in missing information")]
    AnalyzeFrames,
    #[cli(name = "bitexact", flags(GLOBAL), kind = None, help = "restrict output to bit-exact operations")]
    Bitexact,
    #[cli(name = "read_intervals", argname = "read_intervals", flags(HAS_ARG, GLOBAL), kind = Custom, help = "restrict reading to these intervals")]
    ReadIntervals,
    #[cli(name = "i", argname = "input_file", flags(HAS_ARG, PER_FILE, INPUT, OPENS_INPUT), kind = Str, help = "read from this input URL")]
    I,
    #[cli(name = "o", argname = "output_file", flags(HAS_ARG, GLOBAL), kind = Str, help = "write output to this file")]
    O,
    #[cli(name = "print_filename", argname = "print_file", flags(HAS_ARG, GLOBAL), kind = Str, help = "override the filename printed in the output")]
    PrintFilename,
    #[cli(name = "find_stream_info", flags(GLOBAL), kind = None, help = "probe the input before opening it")]
    FindStreamInfo,
    #[cli(name = "c", argname = "decoder_name", flags(HAS_ARG, GLOBAL), kind = Str, help = "force a decoder")]
    C,
    #[cli(name = "codec", alias_of = "c")]
    Codec,
}

pub(crate) static FFPROBE_OPTIONS: &[OptDesc] = FfprobeOptions::OPTIONS;
