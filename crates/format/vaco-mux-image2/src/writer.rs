//! `image2`: the pattern/`-update`/`-strftime`/`-atomic_writing` file writer.
//!
//! # The registry seam does not fit this format, either
//!
//! [`vaco_format_core::MuxerDesc::open`] is
//! `fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>` — one already-open
//! sink, no filename, so it cannot express "open a new file per frame."
//! [`crate::pipe_mux::MUXER_IMAGE2`] is what the registry path gets instead:
//! every frame's payload written consecutively into the one given sink,
//! which is `image2pipe`'s own shape (and, on the read side, exactly what
//! `vaco-demux-image2`'s pipe splitters expect to be fed). The *real*
//! per-file writer below is for a caller that has a path pattern to give it
//! directly.

use std::fs;
use std::path::{Path, PathBuf};

use vaco_core::{Error, Result};
use vaco_demux_image2::pattern::SequencePattern;

use crate::strftime;

/// Options private to `image2`'s mux side, read directly by
/// [`Image2MuxWriter::create`] — the same "registry has no options
/// parameter" gap `vaco-demux-raw` and `vaco-demux-image2` both document,
/// generalised to the write side.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option; grouping them would break the 1:1 mapping the CLI needs"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image2MuxOptions {
    /// `-update 1`: overwrite one fixed filename every frame, rather than
    /// numbering. Measured (`ffmpeg -update 1 -f image2 upd.png`): the
    /// pattern is used verbatim, with no `%d` substitution at all.
    pub update: bool,
    /// `-start_number`, default `1` (the mux side's own default — distinct
    /// from the demux side's `0`, measured on both `-h` pages).
    pub start_number: i64,
    /// `-strftime 1`: expand the pattern through [`crate::strftime`] instead
    /// of numbering.
    pub strftime: bool,
    /// `-frame_pts 1`: substitute the packet's own PTS for the sequence
    /// number, instead of a monotonic counter.
    pub frame_pts: bool,
    /// `-atomic_writing 1`: write to a temporary name and rename into place.
    ///
    /// The reference's exact temporary-name scheme could not be observed in
    /// this sandbox (no filesystem tracer available; see
    /// `docs/format/vaco-mux-image2.md`). This implementation appends
    /// `.tmp` to the final name, which gets the property that matters —
    /// a reader never observes a partially-written file — even though the
    /// literal temporary name likely differs from the reference's.
    pub atomic_writing: bool,
}

impl Default for Image2MuxOptions {
    fn default() -> Self {
        Self {
            update: false,
            start_number: 1,
            strftime: false,
            frame_pts: false,
            atomic_writing: false,
        }
    }
}

/// The real `image2` writer: one call per frame, one file per call (or one
/// file total, under `-update`).
#[derive(Debug)]
pub struct Image2MuxWriter {
    dir: PathBuf,
    /// The pattern with any leading directory removed. `write_frame` joins
    /// [`Image2MuxWriter::dir`] onto whatever `filename_for` returns, so the
    /// three branches that do not go through [`SequencePattern::format`] have
    /// to yield a bare name too — returning the whole pattern joined `enc` to
    /// `enc/out.png` and failed with `No such file or directory` for every
    /// output path that named a directory at all.
    name: String,
    seq: Option<SequencePattern>,
    options: Image2MuxOptions,
    next_number: i64,
    frames_written: u64,
}

impl Image2MuxWriter {
    /// Prepare a writer for `pattern`. Does not touch the filesystem until
    /// [`Image2MuxWriter::write_frame`] is called.
    ///
    /// A `pattern` with no `%d`/`%0Nd` placeholder is not an error here — the
    /// reference accepts a bare filename for a single still (`-f image2
    /// out.png` writes exactly `out.png`) and only fails the *second* write to
    /// that name (`filename_for` reports it, mirroring `ffmpeg`'s "Cannot
    /// write more than one file with the same name"). A pattern with more
    /// than one placeholder is still rejected here, since numbering it would
    /// be a guess about which one the caller meant.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if numbering is required (`update` and
    /// `strftime` are both off) and `pattern` names more than one placeholder.
    pub fn create(pattern: &str, options: Image2MuxOptions) -> Result<Self> {
        let (dir, name) = vaco_demux_image2::fsutil::split_dir_and_name(pattern);
        let needs_sequence = !options.update && !options.strftime;
        let seq = if needs_sequence && SequencePattern::has_placeholder(name) {
            Some(SequencePattern::parse(name)?)
        } else {
            None
        };
        Ok(Self {
            dir,
            name: name.to_owned(),
            seq,
            next_number: options.start_number,
            options,
            frames_written: 0,
        })
    }

    fn filename_for(&mut self, pts: Option<i64>) -> Result<String> {
        if self.options.update {
            // The pattern *is* the filename; no substitution at all
            // (measured: `-update 1 -f image2 upd.png` writes exactly
            // `upd.png`, never `upd1.png`).
            return Ok(self.name.clone());
        }
        if self.options.strftime {
            return strftime::expand_now(&self.name);
        }
        let Some(seq) = &self.seq else {
            // No placeholder and neither `-update` nor `-strftime`: the
            // pattern is a literal filename, legal for exactly one frame.
            // Measured (`ffmpeg -f image2 out.png` on a 3-frame source):
            // the first frame writes `out.png`, the second fails with
            // "Cannot write more than one file with the same name."
            if self.frames_written > 0 {
                return Err(Error::InvalidData(
                    "image2 mux: cannot write more than one file with the same name; \
                     use -update or a sequence pattern",
                ));
            }
            return Ok(self.name.clone());
        };
        let index = if self.options.frame_pts {
            pts.unwrap_or(0)
        } else {
            let n = self.next_number;
            self.next_number = self.next_number.saturating_add(1);
            n
        };
        Ok(seq.format(index))
    }

    /// Write one frame's already-encoded bytes to its file.
    ///
    /// # Errors
    /// Propagates the underlying [`std::io::Error`]; [`Error::InvalidData`]
    /// per [`Image2MuxWriter::create`]'s numbering requirement.
    pub fn write_frame(&mut self, bytes: &[u8], pts: Option<i64>) -> Result<()> {
        let name = self.filename_for(pts)?;
        let path = self.dir.join(name);
        if self.options.atomic_writing {
            write_atomically(&path, bytes)?;
        } else {
            fs::write(&path, bytes).map_err(Error::from)?;
        }
        self.frames_written = self.frames_written.saturating_add(1);
        Ok(())
    }

    /// How many frames [`Image2MuxWriter::write_frame`] has written so far.
    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.frames_written
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).map_err(Error::from)?;
    fs::rename(&tmp, path).map_err(Error::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vaco-mux-image2-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every branch of `filename_for` is joined onto `dir` afterwards, so a
    /// pattern that names a directory must not carry it into the name as
    /// well — `enc/out.png` used to be written as `enc/enc/out.png`.
    #[test]
    fn an_output_path_with_a_directory_writes_one_file_in_that_directory() {
        for (pattern_name, options) in [
            ("out.png", Image2MuxOptions::default()),
            (
                "upd.png",
                Image2MuxOptions {
                    update: true,
                    ..Image2MuxOptions::default()
                },
            ),
        ] {
            let dir = scratch_dir(&format!("subdir-{pattern_name}"));
            let nested = dir.join("nested");
            fs::create_dir_all(&nested).unwrap();
            let pattern = nested.join(pattern_name);
            let mut w =
                Image2MuxWriter::create(pattern.to_str().unwrap(), options).unwrap();
            w.write_frame(b"bytes", None).unwrap();
            assert_eq!(fs::read(&pattern).unwrap(), b"bytes", "{pattern_name}");
        }
    }

    #[test]
    fn sequence_mode_numbers_from_start_number() {
        let dir = scratch_dir("seq");
        let pattern = dir.join("out%03d.png");
        let mut w = Image2MuxWriter::create(
            pattern.to_str().unwrap(),
            Image2MuxOptions {
                start_number: 5,
                ..Image2MuxOptions::default()
            },
        )
        .unwrap();
        w.write_frame(b"a", None).unwrap();
        w.write_frame(b"b", None).unwrap();
        assert_eq!(fs::read(dir.join("out005.png")).unwrap(), b"a");
        assert_eq!(fs::read(dir.join("out006.png")).unwrap(), b"b");
        assert_eq!(w.frames_written(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_mode_overwrites_one_fixed_file() {
        let dir = scratch_dir("update");
        let path = dir.join("upd.png");
        let mut w = Image2MuxWriter::create(
            path.to_str().unwrap(),
            Image2MuxOptions {
                update: true,
                ..Image2MuxOptions::default()
            },
        )
        .unwrap();
        w.write_frame(b"first", None).unwrap();
        w.write_frame(b"second", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        // No numbered siblings were created.
        let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn frame_pts_names_files_by_timestamp() {
        let dir = scratch_dir("pts");
        let pattern = dir.join("pts-%d.png");
        let mut w = Image2MuxWriter::create(
            pattern.to_str().unwrap(),
            Image2MuxOptions {
                frame_pts: true,
                ..Image2MuxOptions::default()
            },
        )
        .unwrap();
        w.write_frame(b"x", Some(0)).unwrap();
        w.write_frame(b"y", Some(7)).unwrap();
        assert!(dir.join("pts-0.png").is_file());
        assert!(dir.join("pts-7.png").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_writing_leaves_no_tmp_file_behind() {
        let dir = scratch_dir("atomic");
        let pattern = dir.join("out%d.png");
        let mut w = Image2MuxWriter::create(
            pattern.to_str().unwrap(),
            Image2MuxOptions {
                atomic_writing: true,
                ..Image2MuxOptions::default()
            },
        )
        .unwrap();
        w.write_frame(b"data", None).unwrap();
        assert_eq!(fs::read(dir.join("out1.png")).unwrap(), b"data");
        assert!(!dir.join("out1.png.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_filename_writes_once_then_refuses_a_second_write() {
        // Measured (`ffmpeg -f image2 out.png` on a multi-frame source): the
        // first frame writes `out.png` literally; the second fails with
        // "Cannot write more than one file with the same name."
        let dir = scratch_dir("bare_filename");
        let path = dir.join("out.png");
        let mut w =
            Image2MuxWriter::create(path.to_str().unwrap(), Image2MuxOptions::default()).unwrap();
        w.write_frame(b"one", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert!(w.write_frame(b"two", None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_still_rejects_more_than_one_placeholder() {
        assert!(Image2MuxWriter::create("a%d_%d.png", Image2MuxOptions::default()).is_err());
    }

    #[test]
    fn update_mode_needs_no_placeholder() {
        let dir = scratch_dir("update_no_placeholder");
        let path = dir.join("out.png");
        assert!(
            Image2MuxWriter::create(
                path.to_str().unwrap(),
                Image2MuxOptions {
                    update: true,
                    ..Image2MuxOptions::default()
                }
            )
            .is_ok()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
