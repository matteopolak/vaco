//! Encoder pass setup and persistence of opaque first-pass statistics.

use std::path::PathBuf;

use vaco_codec_core::{Encoder, EncoderPass};
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_packet::Packet;

/// One output stream's requested rate-control pass and statistics file.
#[derive(Debug, Clone, Default)]
pub struct PassConfig {
    /// Zero is ordinary encoding; one writes statistics, two consumes them.
    pub number: u8,
    /// Prefix combined with the global output-stream index at pipeline setup.
    pub prefix: String,
}

/// Configure an encoder before feeding it and persist pass-one statistics only
/// after it has successfully drained. Unsupported encoders fail before writing.
pub(crate) fn configure(
    mut encoder: Box<dyn Encoder>,
    config: Option<&PassConfig>,
    stream_index: usize,
) -> Result<Box<dyn Encoder>> {
    let Some(config) = config.filter(|c| c.number != 0) else {
        return Ok(encoder);
    };
    let logfile = PathBuf::from(format!("{}-{stream_index}.log", config.prefix));
    let pass = if config.number == 1 {
        EncoderPass::First
    } else {
        const MAX_STATS_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::File::open(&logfile).map_err(Error::Io)?;
        let mut stats = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_STATS_BYTES + 1),
            &mut stats,
        )
        .map_err(Error::Io)?;
        if stats.len() as u64 > MAX_STATS_BYTES {
            return Err(Error::InvalidData("two-pass statistics exceed 64 MiB"));
        }
        EncoderPass::Second(stats)
    };
    encoder.set_pass(pass)?;
    Ok(Box::new(PassEncoder {
        inner: encoder,
        destination: (config.number == 1).then_some(logfile),
    }))
}

struct PassEncoder {
    inner: Box<dyn Encoder>,
    destination: Option<PathBuf>,
}

impl Encoder for PassEncoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        self.inner.send_frame(frame)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        let result = self.inner.receive_packet();
        if matches!(result, Err(Error::Eof))
            && let Some(path) = self.destination.as_ref()
        {
            let stats = self.inner.pass_stats()?.ok_or(Error::InvalidData(
                "encoder completed pass one without producing statistics",
            ))?;
            std::fs::write(path, stats).map_err(Error::Io)?;
            self.destination = None;
        }
        result
    }

    fn flush(&mut self) {
        self.inner.flush();
    }
    fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
        self.inner.set_pass(pass)
    }
    fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
        self.inner.pass_stats()
    }
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        self.inner.set_option(key, value)
    }
    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        self.inner.accepted_pix_fmts()
    }
    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        self.inner.accepted_sample_fmts()
    }
    fn prime_audio(
        &mut self,
        rate: u32,
        layout: vaco_chlayout::ChannelLayout,
        format: vaco_sampfmt::SampleFmt,
    ) {
        self.inner.prime_audio(rate, layout, format);
    }
    fn extradata(&self) -> Option<Vec<u8>> {
        self.inner.extradata()
    }
}
