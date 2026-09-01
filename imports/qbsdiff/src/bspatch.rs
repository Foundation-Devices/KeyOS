#![forbid(unsafe_code)]

use std::io::{Cursor, Error, ErrorKind, Read, Result, Seek, SeekFrom, Write};

#[cfg(feature = "bzip2")]
use bzip2::read::BzDecoder;

use super::utils::*;

/// Default buffer size.
pub const BUFFER_SIZE: usize = 131072;

/// Default initial size of the delta calculation buffer.
pub const DELTA_MIN: usize = 32768;

/// Largest zstd window each section may ask for. The decoder allocates one per
/// section and holds all three for the whole patch, so this is what bounds its
/// memory against a hostile patch.
///
/// They differ because the sections' redundancy does: control records and delta
/// bytes only match within a few hundred kilobytes, so a wider window finds them
/// nothing, while extra is literal new content whose matches sit megabytes apart
/// and shrinks 26% between 20 and 22. One value for all three would either
/// overpay on two sections or starve the only one that pays.
///
/// Extra stops at 22 rather than 23, which would be another 3.8% off a whole
/// update but four more megabytes of window on a device that has none to spare.
///
/// Also the encoder's ceiling, so a patch that would not apply cannot be built.
pub(crate) const MAX_WINDOW_LOGS: [u32; 3] = [20, 20, 22];

/// Fast and memory saving patcher compatible with bspatch.
///
/// Apply patch with a 4k copy buffer and a 1k-4k delta cache buffer:
/// ```
/// use std::io;
///
/// use qbsdiff::Bspatch;
///
/// fn bspatch(source: &[u8], patch: &[u8]) -> io::Result<Vec<u8>> {
///     let mut target = Vec::new();
///     let mut scratch = vec![0; 65536];
///     Bspatch::new(patch)?
///         .buffer_size(4096)
///         .delta_min(1024)
///         .apply(io::Cursor::new(source), &mut scratch, io::Cursor::new(&mut target))?;
///     Ok(target)
/// }
/// ```
///
/// Preallocate target file before applying patch:
/// ```
/// use std::fs::File;
/// use std::io;
/// use std::path::Path;
///
/// use qbsdiff::Bspatch;
///
/// fn file_allocate(file: &mut File, size: u64) -> io::Result<()> { unimplemented!() }
///
/// fn bspatch<P: AsRef<Path>>(source: &[u8], target: P, patch: &[u8]) -> io::Result<u64> {
///     let patcher = Bspatch::new(patch)?;
///     let mut target_file = File::create(target)?;
///     file_allocate(&mut target_file, patcher.hint_target_size())?;
///     let mut scratch = vec![0; 65536];
///     patcher.apply(io::Cursor::new(source), &mut scratch, &mut target_file)
/// }
/// ```
pub struct Bspatch<R: Read> {
    patch: PatchFile<R>,
    buffer_size: usize,
    delta_min: usize,
}

impl<'p> Bspatch<std::io::Take<Cursor<&'p [u8]>>> {
    /// Parse the patch file and create new patcher configuration.
    ///
    /// Return error if failed to parse the patch header.
    pub fn new(patch: &'p [u8]) -> Result<Self> {
        let readers = [Cursor::new(patch), Cursor::new(patch), Cursor::new(patch)];
        Self::from_readers(readers, patch.len() as u64)
    }
}

impl<R: Read + Seek> Bspatch<std::io::Take<R>> {
    /// parse a patch from three independent readers at the same position
    pub fn from_readers(readers: [R; 3], patch_size: u64) -> Result<Self> {
        Ok(Self { patch: parse_readers(readers, patch_size)?, buffer_size: BUFFER_SIZE, delta_min: DELTA_MIN })
    }
}

impl<R: Read> Bspatch<R> {
    /// Set the main copy buffer size, (`bs > 128`, default is `BUFFER_SIZE`).
    ///
    /// This is also the write buffer to target stream.
    /// A relative big buffer (usually 128k) will speed up writing process
    /// if the target stream is unbuffered (e.g. `std::fs::File`).
    pub fn buffer_size(mut self, mut bs: usize) -> Self {
        if bs < 128 {
            bs = 128;
        }
        self.buffer_size = bs;
        self
    }

    /// Sets the initial delta cache size, (`dm > 128`, default is `DELTA_MIN`).
    ///
    /// The delta cache is dynamic and can grow up when needed (but keeps not
    /// greater than the size of main copy buffer).
    ///
    /// This might be deprecated in later version.
    pub fn delta_min(mut self, mut dm: usize) -> Self {
        if dm < 128 {
            dm = 128;
        }
        self.delta_min = dm;
        self
    }

    /// Hint the final target file size.
    pub fn hint_target_size(&self) -> u64 { self.patch.tsize }

    /// Apply patch to the source data and output the stream of target.
    ///
    /// The source is read rather than taken as a `&[u8]`, because a firmware
    /// image does not fit in the memory of the device patching it. Upstream
    /// argues for a slice on the grounds that a seek and a read per record costs
    /// more than it saves, which is why a batch is sorted and merged into runs
    /// before any of it is read.
    ///
    /// `scratch` holds one run, so its length is the longest read that will be
    /// issued. Callers that can allocate a page-aligned one should, since a
    /// filesystem may be able to lend it rather than copy through it.
    ///
    /// The target data size would be returned if no error occurs.
    pub fn apply<S: Read + Seek, T: Write>(self, source: S, scratch: &mut [u8], target: T) -> Result<u64> {
        let delta_min = Ord::min(self.delta_min, self.buffer_size);
        Context::new(self.patch, source, target, self.buffer_size, delta_min).apply(scratch)
    }
}

/// Patch file content.
struct PatchFile<R: Read> {
    tsize: u64,
    ctrls: Stream<R>,
    delta: Stream<R>,
    extra: Stream<R>,
}

/// One of the three compressed sections, decompressed on the fly.
enum Stream<R: Read> {
    #[cfg(feature = "bzip2")]
    Bzip2(BzDecoder<R>),
    #[cfg(feature = "zstd")]
    // Boxed because a FrameDecoder carries its own tables, and three of them
    // inline would dwarf the rest of the context.
    Zstd(Box<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>),
}

impl<R: Read> Stream<R> {
    fn new(codec: Codec, section: Section, raw: R) -> Result<Self> {
        match codec {
            #[cfg(feature = "bzip2")]
            Codec::Bzip2 => {
                let _ = section;
                Ok(Stream::Bzip2(BzDecoder::new(raw)))
            }
            #[cfg(not(feature = "bzip2"))]
            Codec::Bzip2 => Err(Error::new(ErrorKind::InvalidData, "bzip2 patches are not supported")),
            #[cfg(feature = "zstd")]
            Codec::Zstd => {
                let decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                    raw,
                    1 << MAX_WINDOW_LOGS[section as usize],
                )
                .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
                Ok(Stream::Zstd(Box::new(decoder)))
            }
            #[cfg(not(feature = "zstd"))]
            Codec::Zstd => Err(Error::new(ErrorKind::InvalidData, "zstd patches are not supported")),
        }
    }
}

impl<R: Read> Read for Stream<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            #[cfg(feature = "bzip2")]
            Stream::Bzip2(reader) => reader.read(buf),
            #[cfg(feature = "zstd")]
            Stream::Zstd(reader) => reader.read(buf),
        }
    }
}

/// One of the three compressed sections of a patch, in the order they are
/// stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Ctrl = 0,
    Delta = 1,
    Extra = 2,
}

/// Which codec compressed the three sections, named by the patch magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Bzip2,
    Zstd,
}

impl Codec {
    pub(crate) const fn magic(self) -> &'static [u8; 8] {
        match self {
            Codec::Bzip2 => b"BSDIFF40",
            Codec::Zstd => b"BSDIFF4Z",
        }
    }

    fn from_magic(magic: &[u8]) -> Option<Self> {
        [Codec::Bzip2, Codec::Zstd].into_iter().find(|codec| magic == codec.magic())
    }
}

#[cfg(test)]
fn parse(patch: &[u8]) -> Result<PatchFile<std::io::Take<Cursor<&[u8]>>>> {
    parse_readers([Cursor::new(patch), Cursor::new(patch), Cursor::new(patch)], patch.len() as u64)
}

/// Parse the bsdiff 4.x patch file.
fn parse_readers<R: Read + Seek>(readers: [R; 3], patch_size: u64) -> Result<PatchFile<std::io::Take<R>>> {
    if patch_size < 32 {
        return Err(Error::new(ErrorKind::InvalidData, "not a valid patch"));
    }

    let [mut ctrls, mut delta, mut extra] = readers;
    let delta_start = delta.stream_position()?;
    let extra_start = extra.stream_position()?;
    let mut header = [0; 32];
    ctrls.read_exact(&mut header)?;

    let codec = Codec::from_magic(&header[..8])
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "not a valid patch"))?;
    let csize = decode_int(&header[8..16]) as u64;
    let dsize = decode_int(&header[16..24]) as u64;
    let tsize = decode_int(&header[24..32]) as u64;
    let extra_offset = 32u64
        .checked_add(csize)
        .and_then(|offset| offset.checked_add(dsize))
        .filter(|offset| *offset <= patch_size)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "patch corrupted"))?;

    delta.seek(SeekFrom::Start(delta_start + 32 + csize))?;
    extra.seek(SeekFrom::Start(extra_start + extra_offset))?;

    let ctrls = Stream::new(codec, Section::Ctrl, ctrls.take(csize))?;
    let delta = Stream::new(codec, Section::Delta, delta.take(dsize))?;
    let extra = Stream::new(codec, Section::Extra, extra.take(patch_size - extra_offset))?;

    Ok(PatchFile { tsize, ctrls, delta, extra })
}

/// Bspatch context.
struct Context<R: Read, S: Read + Seek, T: Write> {
    source: S,
    target: T,

    patch: PatchFile<R>,

    buf: Vec<u8>,
    dlt: Vec<u8>,
    ctl: [u8; 24],

    /// Remainder of the control the buffer cut in half, opening the next batch.
    pending: Option<Control>,
    source_pos: i64,

    total: u64,
}

/// One bufferful of target, planned before any source byte is read.
struct Batch {
    /// Add and copy lengths, in target order.
    segments: Vec<(usize, usize)>,
    reads: Vec<SourceRead>,
    used: usize,
}

/// One source read gathered while planning a batch.
#[derive(Debug, Clone, Copy)]
struct SourceRead {
    source_offset: u64,
    buffer_offset: usize,
    len: usize,
}

/// Neighbouring reads closer than this are pulled together. A read costs far
/// more than the bytes it carries, so reading across a small gap beats paying
/// for a second one.
const COALESCE_GAP: u64 = 8192;

impl<R: Read, S: Read + Seek, T: Write> Context<R, S, T> {
    /// Create context.
    fn new(patch: PatchFile<R>, source: S, target: T, bsize: usize, dsize: usize) -> Self {
        Context {
            source,
            target,
            patch,
            buf: vec![0; bsize],
            dlt: vec![0; dsize],
            ctl: [0; 24],
            pending: None,
            source_pos: 0,
            total: 0,
        }
    }

    /// Apply the patch file, a bufferful of target at a time.
    fn apply(mut self, scratch: &mut [u8]) -> Result<u64> {
        loop {
            let Batch { segments, reads, used } = self.plan()?;
            if used == 0 {
                break;
            }
            self.gather(scratch, reads)?;
            self.overlay(segments)?;

            self.target.write_all(&self.buf[..used])?;
            self.total += used as u64;
        }

        self.target.flush()?;
        Ok(self.total)
    }

    /// Plan the next batch: the source bytes it needs and the segments laying
    /// the delta and extra over them. `used` comes back zero once the patch runs
    /// out.
    fn plan(&mut self) -> Result<Batch> {
        let mut segments = Vec::new();
        let mut reads = Vec::new();
        let mut used = 0;

        while used < self.buf.len() {
            let ctrl = match self.pending.take() {
                Some(ctrl) => ctrl,
                None => match self.next_control() {
                    Some(ctrl) => ctrl?,
                    None => break,
                },
            };

            let add = Ord::min(ctrl.add, (self.buf.len() - used) as u64) as usize;
            if add > 0 {
                reads.push(SourceRead {
                    source_offset: self.source_pos as u64,
                    buffer_offset: used,
                    len: add,
                });
                used += add;
                self.source_pos += add as i64;
            }
            let copy = Ord::min(ctrl.copy, (self.buf.len() - used) as u64) as usize;
            used += copy;
            // A control that advances nothing still ends a parallel patch, and a
            // long run of them would grow the batch while it never fills.
            if add > 0 || copy > 0 {
                segments.push((add, copy));
            }

            if (add as u64) < ctrl.add || (copy as u64) < ctrl.copy {
                self.pending = Some(Control {
                    add: ctrl.add - add as u64,
                    copy: ctrl.copy - copy as u64,
                    seek: ctrl.seek,
                });
                break;
            }
            self.source_pos += ctrl.seek;
        }

        Ok(Batch { segments, reads, used })
    }

    /// Read the planned source bytes into the buffer, merging neighbours into
    /// runs no longer than `scratch`.
    ///
    /// Sorted first, because a filesystem walking a cluster chain answers
    /// ascending offsets far more cheaply than the order bsdiff emits them in.
    fn gather(&mut self, scratch: &mut [u8], mut reads: Vec<SourceRead>) -> Result<()> {
        reads.sort_unstable_by_key(|read| read.source_offset);

        let mut next = 0;
        while next < reads.len() {
            let first = reads[next];
            let mut end = first.source_offset + first.len as u64;
            let mut last = next;
            while let Some(candidate) = reads.get(last + 1) {
                // Bound the run, not the candidate: a read longer than the scratch
                // would otherwise still absorb a short one lying inside it.
                let run_end = end.max(candidate.source_offset + candidate.len as u64);
                if candidate.source_offset > end + COALESCE_GAP
                    || run_end - first.source_offset > scratch.len() as u64
                {
                    break;
                }
                end = run_end;
                last += 1;
            }

            self.source.seek(SeekFrom::Start(first.source_offset))?;
            if last == next {
                // Nothing merged, so it can land where it belongs. This is also the
                // only path a read longer than the scratch can take.
                self.source.read_exact(&mut self.buf[first.buffer_offset..][..first.len])?;
            } else {
                let run = (end - first.source_offset) as usize;
                self.source.read_exact(&mut scratch[..run])?;
                for read in &reads[next..=last] {
                    let from = (read.source_offset - first.source_offset) as usize;
                    self.buf[read.buffer_offset..][..read.len]
                        .copy_from_slice(&scratch[from..from + read.len]);
                }
            }
            next = last + 1;
        }
        Ok(())
    }

    /// Add the delta onto the source bytes in the buffer and fill the gaps
    /// between them with extra.
    fn overlay(&mut self, segments: Vec<(usize, usize)>) -> Result<()> {
        let mut offset = 0;
        for (add, copy) in segments {
            let mut remaining = add;
            while remaining > 0 {
                let len = Ord::min(remaining, self.dlt.len());
                self.patch.delta.read_exact(&mut self.dlt[..len])?;
                Iterator::zip(self.buf[offset..offset + len].iter_mut(), self.dlt[..len].iter())
                    .for_each(|(x, y)| *x = x.wrapping_add(*y));
                offset += len;
                remaining -= len;
            }
            self.patch.extra.read_exact(&mut self.buf[offset..offset + copy])?;
            offset += copy;
        }
        Ok(())
    }

    /// Read the next control.
    fn next_control(&mut self) -> Option<Result<Control>> {
        match read_exact_or_eof(&mut self.patch.ctrls, &mut self.ctl[..]) {
            Ok(0) => return None,
            Err(e) => return Some(Err(e)),
            _ => (),
        }

        let add = decode_int(&self.ctl[0..]) as u64;
        let copy = decode_int(&self.ctl[8..]) as u64;
        let seek = decode_int(&self.ctl[16..]);
        Some(Ok(Control { add, copy, seek }))
    }
}

// Read exact buf.len() bytes or reads an EOF, return read bytes count.
#[inline]
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut cnt = 0;
    while cnt < buf.len() {
        match r.read(&mut buf[cnt..]) {
            Ok(0) => break,
            Ok(n) => cnt += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    if cnt != 0 && cnt != buf.len() {
        Err(Error::new(ErrorKind::UnexpectedEof, "failed to fill whole buffer"))
    } else {
        Ok(cnt)
    }
}

#[cfg(all(test, feature = "encode"))]
mod coalesce_tests {
    use std::io::sink;

    use super::*;

    fn read(source_offset: u64, buffer_offset: usize, len: usize) -> SourceRead {
        SourceRead { source_offset, buffer_offset, len }
    }

    /// Merging must land the same bytes as a seek and a read per record, whatever
    /// it decides to merge.
    fn check(scratch_len: usize, reads: &[SourceRead]) {
        let source: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let out_len = reads.iter().map(|r| r.buffer_offset + r.len).max().unwrap_or(0);

        let mut expected = vec![0u8; out_len];
        for r in reads {
            expected[r.buffer_offset..][..r.len]
                .copy_from_slice(&source[r.source_offset as usize..][..r.len]);
        }

        // Gathering never touches the patch streams, so an empty patch will do.
        let mut patch = Vec::new();
        crate::Bsdiff::new(b"", b"").compare(&mut patch).unwrap();
        let mut ctx = Context::new(parse(&patch).unwrap(), Cursor::new(&source), sink(), out_len, 128);

        ctx.gather(&mut vec![0; scratch_len], reads.to_vec()).unwrap();
        assert_eq!(ctx.buf, expected);
    }

    #[test]
    fn merges_neighbours() { check(256, &[read(0, 0, 64), read(64, 64, 64), read(128, 128, 64)]); }

    #[test]
    fn reads_through_a_gap_within_the_tolerance() { check(16384, &[read(0, 0, 64), read(4096, 64, 64)]); }

    #[test]
    fn a_wider_gap_starts_a_new_run() {
        check(16384, &[read(0, 0, 64), read(COALESCE_GAP + 100, 64, 64)]);
    }

    #[test]
    fn a_run_never_outgrows_the_scratch() {
        let reads: Vec<_> = (0..64).map(|i| read(i * 64, i as usize * 64, 64)).collect();
        check(256, &reads);
    }

    /// A read longer than the scratch has to be read on its own.
    #[test]
    fn an_oversize_read_stands_alone() { check(64, &[read(0, 0, 500)]); }

    /// It must stay alone even when a short read sits inside its extent, which
    /// leaves the candidate's own end within the bound.
    #[test]
    fn an_oversize_read_does_not_absorb_one_inside_it() { check(64, &[read(0, 0, 500), read(8, 500, 16)]); }

    #[test]
    fn overlapping_reads_do_not_shrink_the_run() {
        check(256, &[read(0, 0, 200), read(64, 200, 16), read(100, 216, 100)]);
    }

    #[test]
    fn handles_one_read_and_none() {
        check(256, &[read(300, 0, 32)]);
        check(256, &[]);
    }
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn codec_is_named_by_the_magic() {
        assert_eq!(Codec::from_magic(b"BSDIFF40"), Some(Codec::Bzip2));
        assert_eq!(Codec::from_magic(b"BSDIFF4Z"), Some(Codec::Zstd));
        assert_eq!(Codec::from_magic(b"BSDIFF41"), None);
    }

    #[cfg(all(feature = "zstd", feature = "zstd-encode"))]
    #[test]
    fn both_codecs_round_trip() {
        let source = b"the quick brown fox jumps over the lazy dog".repeat(64);
        let target = b"the quick brown cat jumps over the lazy dog".repeat(64);

        for codec in [Codec::Bzip2, Codec::Zstd] {
            let mut patch = Vec::new();
            crate::Bsdiff::new(&source, &target).codec(codec).compare(&mut patch).unwrap();
            assert_eq!(&patch[..8], codec.magic(), "wrong magic for {codec:?}");

            let readers = [Cursor::new(&patch), Cursor::new(&patch), Cursor::new(&patch)];
            let mut out = Vec::new();
            Bspatch::from_readers(readers, patch.len() as u64)
                .unwrap()
                .apply(Cursor::new(&source), &mut [0; 4096], &mut out)
                .unwrap();
            assert_eq!(out, target, "{codec:?} did not round trip");
        }
    }

    fn xorshift(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }

    /// A control section that decodes to a long run of records advancing nothing
    /// must not grow the batch, whatever it costs in time.
    #[cfg(feature = "bzip2")]
    #[test]
    fn controls_advancing_nothing_do_not_accumulate() {
        use std::io::sink;

        fn bzip2(data: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            bzip2::read::BzEncoder::new(data, bzip2::Compression::best()).read_to_end(&mut out).unwrap();
            out
        }

        let mut record = [0u8; 24];
        encode_int(0, &mut record[0..8]);
        encode_int(0, &mut record[8..16]);
        encode_int(-16, &mut record[16..24]);
        let ctrls = bzip2(&record.repeat(10_000));

        let mut patch = Vec::from(Codec::Bzip2.magic().as_slice());
        let mut field = [0u8; 8];
        encode_int(ctrls.len() as i64, &mut field);
        patch.extend_from_slice(&field);
        encode_int(0, &mut field);
        patch.extend_from_slice(&field);
        patch.extend_from_slice(&field);
        patch.extend_from_slice(&ctrls);

        let mut ctx = Context::new(parse(&patch).unwrap(), Cursor::new(&[][..]), sink(), 4096, 128);
        let batch = ctx.plan().unwrap();

        assert_eq!(batch.used, 0);
        assert!(batch.segments.is_empty(), "kept {} empty segments", batch.segments.len());
    }

    /// A control split across batches must not apply its seek until the remainder
    /// is consumed, or whatever follows reads from the wrong source offset.
    #[test]
    fn a_split_control_defers_its_seek() {
        let mut state = 0x9E37_79B9;
        let source: Vec<u8> = (0..16 * 1024).map(|_| xorshift(&mut state) as u8).collect();
        // Swapped halves, so the patch is two adds far longer than the buffer
        // below with a backward seek between them.
        let target = [&source[8192..], &source[..8192]].concat();

        let mut patch = Vec::new();
        crate::Bsdiff::new(&source, &target).compare(&mut patch).unwrap();

        let mut out = Vec::new();
        Bspatch::new(&patch)
            .unwrap()
            .buffer_size(1024)
            .apply(Cursor::new(&source), &mut [0; 4096], &mut out)
            .unwrap();
        assert_eq!(out, target);
    }

    /// Batching must not change the output, so a buffer small enough to split
    /// most controls has to agree with the target it was diffed from.
    #[test]
    fn a_small_buffer_gives_the_same_target() {
        let mut state = 0x1234_5678;
        let source: Vec<u8> = (0..16 * 1024).map(|_| xorshift(&mut state) as u8).collect();

        // Chunks lifted from scattered source offsets, separated by fresh bytes,
        // so the patch carries seeks, deltas and extras throughout.
        let mut target = Vec::new();
        for _ in 0..64 {
            let offset = xorshift(&mut state) as usize % (source.len() - 512);
            let len = 64 + xorshift(&mut state) as usize % 448;
            target.extend_from_slice(&source[offset..offset + len]);
            for _ in 0..xorshift(&mut state) % 64 {
                target.push(xorshift(&mut state) as u8);
            }
        }

        let mut patch = Vec::new();
        crate::Bsdiff::new(&source, &target).compare(&mut patch).unwrap();

        for (buffer, delta, scratch) in [(128, 128, 128), (333, 200, 512), (4096, 1024, 100)] {
            let mut out = Vec::new();
            Bspatch::new(&patch)
                .unwrap()
                .buffer_size(buffer)
                .delta_min(delta)
                .apply(Cursor::new(&source), &mut vec![0; scratch], &mut out)
                .unwrap();
            assert_eq!(out, target, "buffer {buffer}, delta {delta}, scratch {scratch}");
        }
    }

    #[test]
    fn bspatch_with_file_as_input() {
        struct DropDeleteFile(&'static str);

        impl Drop for DropDeleteFile {
            fn drop(&mut self) {
                if let Err(e) = fs::remove_file(self.0) {
                    eprintln!("failed to delete file `{}`: {}", self.0, e);
                }
            }
        }

        const SOURCE_FILE_PATH: &str = "source";
        const SOURCE_CONTENT: &[u8] = b"Hello, world!";
        const TARGET_CONTENT: &[u8] = b"Hello, Rustaceans!";

        let mut source_file =
            fs::File::options().read(true).write(true).create(true).open(SOURCE_FILE_PATH).unwrap();
        let _drop = DropDeleteFile(SOURCE_FILE_PATH);

        source_file.write_all(SOURCE_CONTENT).unwrap();
        // Seek back to start since we re-use the same file handle as source to apply the patch.
        source_file.seek(SeekFrom::Start(0)).unwrap();
        source_file.flush().unwrap();

        let mut patch = Vec::new();
        let bsdiff = crate::Bsdiff::new(SOURCE_CONTENT, TARGET_CONTENT);
        let _ = bsdiff.compare(&mut patch).unwrap();

        let bspatch = Bspatch::new(&patch).unwrap();

        let mut target = Vec::new();
        let _ = bspatch.apply(source_file, &mut [0; 4096], &mut target).unwrap();

        assert_eq!(&target[..], TARGET_CONTENT);
    }
}
