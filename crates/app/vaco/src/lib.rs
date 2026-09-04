//! Vaco's stable, namespaced public API.
//!
//! This file is generated from `release/vaco-public-api.json`; edit the
//! descriptor and rerun `scripts/gen-vaco-facade.py`, not this output.
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

pub use vaco_cli as cli;
pub use vaco_probe as probe;
pub use vaco_registry as registry;

pub mod application {
    pub use vaco_cli_core as cli_core;
    pub use vaco_sched as sched;
    pub use vaco_textformat as textformat;
}

pub mod codec {
    #[cfg(any(feature = "patent-encumbered-aac-decode"))]
    pub use vaco_codec_aac as aac;
    pub use vaco_codec_ac3 as ac3;
    pub use vaco_codec_adpcm as adpcm;
    pub use vaco_codec_alac as alac;
    pub use vaco_codec_exec as exec;
    pub use vaco_codec_exr as exr;
    pub use vaco_codec_ffv1 as ffv1;
    pub use vaco_codec_flac as flac;
    pub use vaco_codec_gif as gif;
    pub use vaco_codec_h263 as h263;
    #[cfg(any(feature = "patent-encumbered-h264-decode"))]
    pub use vaco_codec_h264 as h264;
    #[cfg(any(feature = "patent-encumbered-hevc-decode"))]
    pub use vaco_codec_hevc as hevc;
    pub use vaco_codec_image_simple as image_simple;
    pub use vaco_codec_jpeg as jpeg;
    pub use vaco_codec_jpegls as jpegls;
    pub use vaco_codec_jpegxl as jpegxl;
    pub use vaco_codec_mpeg12 as mpeg12;
    pub use vaco_codec_mpegaudio as mpegaudio;
    pub use vaco_codec_null as null;
    pub use vaco_codec_pcm as pcm;
    pub use vaco_codec_png as png;
    pub use vaco_codec_pnm as pnm;
    pub use vaco_codec_prores as prores;
    pub use vaco_codec_qoi as qoi;
    pub use vaco_codec_rawvideo as rawvideo;
    pub use vaco_codec_simple_audio as simple_audio;
    pub use vaco_codec_subtitle_bitmap as subtitle_bitmap;
    pub use vaco_codec_subtitle_cc as subtitle_cc;
    pub use vaco_codec_subtitle_teletext as subtitle_teletext;
    pub use vaco_codec_subtitle_text as subtitle_text;
    pub use vaco_codec_theora as theora;
    pub use vaco_codec_tiff as tiff;
    #[cfg(any(feature = "patent-encumbered-vc1-decode"))]
    pub use vaco_codec_vc1 as vc1;
    pub use vaco_codec_vorbis as vorbis;
    pub use vaco_codec_vp8 as vp8;
    pub use vaco_codec_vp9 as vp9;
    pub use vaco_codec_webp as webp;
    pub use vaco_parse_aac as parse_aac;
    pub use vaco_parse_audio_misc as parse_audio_misc;
    pub use vaco_parse_av1 as parse_av1;
    pub use vaco_parse_ffv1 as parse_ffv1;
    pub use vaco_parse_h264 as parse_h264;
    pub use vaco_parse_hevc as parse_hevc;
    pub use vaco_parse_image as parse_image;
    pub use vaco_parse_mpegaudio as parse_mpegaudio;
    pub use vaco_parse_mpegvideo as parse_mpegvideo;
    pub use vaco_parse_opus as parse_opus;
    pub use vaco_parse_prores as parse_prores;
    pub use vaco_parse_vpx as parse_vpx;
}

pub mod core {
    pub use vaco_bitstream as bitstream;
    pub use vaco_core as core;
    pub use vaco_crypto as crypto;
    pub use vaco_expr as expr;
    pub use vaco_hash as hash;
    pub use vaco_limits as limits;
    pub use vaco_opts as opts;
    pub use vaco_opts_derive as opts_derive;
    pub use vaco_simd as simd;
    pub use vaco_time as time;
    pub use vaco_vecheck_macros as vecheck_macros;
}

pub mod filter {
    pub use vaco_ass as ass;
    pub use vaco_filter_aanalysis as aanalysis;
    pub use vaco_filter_adsp as adsp;
    pub use vaco_filter_adynamics as adynamics;
    pub use vaco_filter_aeffects as aeffects;
    pub use vaco_filter_aeq as aeq;
    pub use vaco_filter_analysis as analysis;
    pub use vaco_filter_artistic as artistic;
    pub use vaco_filter_asource as asource;
    pub use vaco_filter_audio as audio;
    pub use vaco_filter_blur as blur;
    pub use vaco_filter_color as color;
    pub use vaco_filter_convolve as convolve;
    pub use vaco_filter_core as core;
    pub use vaco_filter_deinterlace as deinterlace;
    pub use vaco_filter_denoise as denoise;
    pub use vaco_filter_draw as draw;
    pub use vaco_filter_draw_vf as draw_vf;
    pub use vaco_filter_framesync as framesync;
    pub use vaco_filter_geometry as geometry;
    pub use vaco_filter_graph as graph;
    pub use vaco_filter_key as key;
    pub use vaco_filter_lut as lut;
    pub use vaco_filter_mm as mm;
    pub use vaco_filter_motion as motion;
    pub use vaco_filter_overlay as overlay;
    pub use vaco_filter_palette as palette;
    pub use vaco_filter_scope as scope;
    pub use vaco_filter_source as source;
    pub use vaco_filter_stack as stack;
    pub use vaco_filter_subtitle as subtitle;
    pub use vaco_filter_temporal as temporal;
    pub use vaco_filter_text as text;
    pub use vaco_filter_v360 as v360;
    pub use vaco_filter_vdsp as vdsp;
    pub use vaco_filter_video_composite as video_composite;
    pub use vaco_filter_video_format as video_format;
    pub use vaco_filter_video_geometry as video_geometry;
    pub use vaco_filter_video_source as video_source;
}

pub mod format {
    pub use vaco_bsf_audio as bsf_audio;
    pub use vaco_bsf_av1 as bsf_av1;
    pub use vaco_bsf_core as bsf_core;
    pub use vaco_bsf_generic as bsf_generic;
    pub use vaco_bsf_h2645 as bsf_h2645;
    pub use vaco_bsf_legacy as bsf_legacy;
    pub use vaco_bsf_subtitle as bsf_subtitle;
    pub use vaco_bsf_vpx as bsf_vpx;
    pub use vaco_demux_asf as demux_asf;
    pub use vaco_demux_avi as demux_avi;
    pub use vaco_demux_dash as demux_dash;
    pub use vaco_demux_flv as demux_flv;
    pub use vaco_demux_hls as demux_hls;
    pub use vaco_demux_image2 as demux_image2;
    pub use vaco_demux_matroska as demux_matroska;
    pub use vaco_demux_mp4 as demux_mp4;
    pub use vaco_demux_mpegaudio as demux_mpegaudio;
    pub use vaco_demux_mpegps as demux_mpegps;
    pub use vaco_demux_mpegts as demux_mpegts;
    pub use vaco_demux_mxf as demux_mxf;
    pub use vaco_demux_ogg as demux_ogg;
    pub use vaco_demux_raw as demux_raw;
    #[cfg(any(feature = "demux-rtp", feature = "demux-rtsp", feature = "demux-sdp"))]
    pub use vaco_demux_rtsp as demux_rtsp;
    pub use vaco_format_ac3 as ac3;
    pub use vaco_format_adaptive as adaptive;
    pub use vaco_format_asf as asf;
    pub use vaco_format_audio_simple as audio_simple;
    pub use vaco_format_core as core;
    pub use vaco_format_dv as dv;
    pub use vaco_format_ebml as ebml;
    pub use vaco_format_gxf as gxf;
    pub use vaco_format_id3 as id3;
    pub use vaco_format_imf as imf;
    pub use vaco_format_isom as isom;
    pub use vaco_format_metadata as metadata;
    pub use vaco_format_misc as misc;
    pub use vaco_format_misc_audio as misc_audio;
    pub use vaco_format_mpegaudio as mpegaudio;
    pub use vaco_format_mpegts_tables as mpegts_tables;
    pub use vaco_format_mpjpeg as mpjpeg;
    pub use vaco_format_nalu as nalu;
    pub use vaco_format_nut as nut;
    pub use vaco_format_riff as riff;
    pub use vaco_format_rtp as rtp;
    pub use vaco_format_spdif as spdif;
    pub use vaco_format_subtitle as subtitle;
    pub use vaco_format_subtitle_bitmap as format_subtitle_bitmap;
    pub use vaco_format_swf as swf;
    pub use vaco_format_vorbiscomment as vorbiscomment;
    pub use vaco_mux_asf as mux_asf;
    pub use vaco_mux_avi as mux_avi;
    pub use vaco_mux_dash as mux_dash;
    pub use vaco_mux_flv as mux_flv;
    pub use vaco_mux_hash as mux_hash;
    pub use vaco_mux_hds as mux_hds;
    pub use vaco_mux_hls as mux_hls;
    pub use vaco_mux_image2 as mux_image2;
    pub use vaco_mux_matroska as mux_matroska;
    pub use vaco_mux_mp4 as mux_mp4;
    pub use vaco_mux_mpegaudio as mux_mpegaudio;
    pub use vaco_mux_mpegps as mux_mpegps;
    pub use vaco_mux_mpegts as mux_mpegts;
    pub use vaco_mux_mxf as mux_mxf;
    pub use vaco_mux_ogg as mux_ogg;
    pub use vaco_mux_raw as mux_raw;
    pub use vaco_mux_rtp as mux_rtp;
    pub use vaco_mux_smoothstreaming as mux_smoothstreaming;
    pub use vaco_mux_stream as mux_stream;
    pub use vaco_mux_utility as mux_utility;
    #[cfg(any(feature = "mux-whip"))]
    pub use vaco_mux_whip as mux_whip;
    pub use vaco_subtitle_bitmap as subtitle_bitmap;
    pub use vaco_subtitle_text as subtitle_text;
}

pub mod io {
    pub use vaco_io as io;
    pub use vaco_protocol_core as core;
    pub use vaco_protocol_crypto as crypto;
    #[cfg(any(feature = "api-dial"))]
    pub use vaco_protocol_dial as dial;
    #[cfg(any(feature = "protocol-dtls"))]
    pub use vaco_protocol_dtls as dtls;
    pub use vaco_protocol_file as file;
    #[cfg(any(feature = "protocol-ftp"))]
    pub use vaco_protocol_ftp as ftp;
    #[cfg(any(feature = "protocol-gopher"))]
    pub use vaco_protocol_gopher as gopher;
    #[cfg(any(feature = "protocol-http"))]
    pub use vaco_protocol_http as http;
    #[cfg(any(feature = "protocol-httpproxy"))]
    pub use vaco_protocol_httpproxy as httpproxy;
    #[cfg(any(feature = "api-ice"))]
    pub use vaco_protocol_ice as ice;
    #[cfg(any(feature = "protocol-icecast"))]
    pub use vaco_protocol_icecast as icecast;
    pub use vaco_protocol_ipfs as ipfs;
    pub use vaco_protocol_local as local;
    pub use vaco_protocol_shared as shared;
    #[cfg(any(feature = "protocol-socket"))]
    pub use vaco_protocol_socket as socket;
    #[cfg(any(feature = "api-srtp"))]
    pub use vaco_protocol_srtp as srtp;
    #[cfg(any(feature = "protocol-tls"))]
    pub use vaco_protocol_tls as tls;
    pub use vaco_protocol_wrap as wrap;
}

pub mod media {
    pub use vaco_chlayout as chlayout;
    pub use vaco_color as color;
    pub use vaco_frame as frame;
    pub use vaco_packet as packet;
    pub use vaco_pixfmt as pixfmt;
    pub use vaco_pool as pool;
    pub use vaco_rtp as rtp;
    pub use vaco_sampfmt as sampfmt;
}

pub mod signal {
    #[cfg(any(feature = "api-codec_cabac"))]
    pub use vaco_codec_cabac as codec_cabac;
    pub use vaco_codec_cbs as codec_cbs;
    pub use vaco_codec_core as codec_core;
    #[cfg(any(feature = "api-codec_dsp_deblock"))]
    pub use vaco_codec_dsp_deblock as codec_dsp_deblock;
    pub use vaco_codec_dsp_idct as codec_dsp_idct;
    pub use vaco_codec_dsp_intrapred as codec_dsp_intrapred;
    pub use vaco_codec_dsp_lpc as codec_dsp_lpc;
    pub use vaco_codec_dsp_me as codec_dsp_me;
    pub use vaco_codec_dsp_mecmp as codec_dsp_mecmp;
    pub use vaco_codec_dsp_ratecontrol as codec_dsp_ratecontrol;
    #[cfg(any(feature = "api-codec_dsp_sinewin"))]
    pub use vaco_codec_dsp_sinewin as codec_dsp_sinewin;
    pub use vaco_codec_golomb as codec_golomb;
    pub use vaco_codec_msac as codec_msac;
    #[cfg(any(feature = "api-codec_vlc"))]
    pub use vaco_codec_vlc as codec_vlc;
    pub use vaco_resample as resample;
    pub use vaco_scale as scale;
    pub use vaco_tx as tx;
}
