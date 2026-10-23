//! Where an alignment viewer should open: the first contig with mapped reads, at its first read, read
//! from the alignment's own index (BAI, CSI or CRAI) and, for the contig's name, the file's own
//! header (a BAM's reference list, a CRAM's SAM header `@SQ` lines). The name never comes from the
//! reference the viewer shows: an index's contig number counts the file's own header, whose order
//! a FASTA need not share.
//!
//! The readers are hand-rolled over the few fields needed (SAM spec §4.2 BAM, §5.2 BAI, the CSI
//! spec, CRAM §7-8 file definition, container and block, §12 CRAI) rather than a full
//! htslib-style reader: every input is a file a pipeline wrote, so each one is read as a stream
//! with bounded reads, bounded inflation and no allocation sized by a count the file claims, and
//! each returns `None` on bad input; nothing here panics. (noodles would add only ~0.3 MB to the
//! Lambda, but its index readers reserve capacity from the file's own counts, e.g.
//! `Vec::with_capacity(n_ref)`, so one corrupt count can abort the process on allocation failure.)
//! The parsing is CPU work of up to [`MAX_INFLATED_INDEX_BYTES`] of inflation, so it runs on a
//! blocking thread, off the async runtime. No locus is never an error: the viewer opens at its own
//! default.

use std::io::{self, BufRead, Read};

use flate2::read::MultiGzDecoder;

use crate::aws::objects::ObjectStore;
use crate::aws::AwsError;

/// The largest index read (a whole-genome BAI is a few MB).
const MAX_INDEX_BYTES: u64 = 32 << 20;
/// The most a compressed index (CSI, CRAI) is inflated to while looking for its first contig.
const MAX_INFLATED_INDEX_BYTES: u64 = 128 << 20;
/// The largest header read, compressed (thousands of contigs fit in far less).
const MAX_HEADER_BYTES: u64 = 8 << 20;
/// The most a header is inflated to while looking for a contig's name: a gzip member is not
/// bounded by BGZF's 64 KiB blocks, so a few MB can claim gigabytes.
const MAX_INFLATED_HEADER_BYTES: u64 = 64 << 20;
/// The first read of a header: enough for almost every file's header.
const FIRST_HEADER_READ: u64 = 1 << 20;
/// Bytes read at a record's block: the block (at most 64 KiB) and the next, since a record's first
/// bytes may cross into it.
const RECORD_READ_BYTES: u64 = 2 << 16;
/// The width of the opening window, in bases.
const LOCUS_WIDTH: u64 = 2000;
/// The longest contig name read (the BAM spec allows up to 2^31; real ones are short).
const MAX_NAME_BYTES: usize = 1 << 16;
/// The longest CRAI line read.
const MAX_CRAI_LINE: u64 = 4096;
/// The longest SAM header line kept while reading a CRAM's `@SQ` lines (longer ones, an `@CO`
/// say, are skipped).
const MAX_SAM_HEADER_LINE: u64 = 1 << 16;
/// The CSI depths whose pseudo-bin number fits the index's 32-bit bin field.
const MAX_CSI_DEPTH: i32 = 10;

/// An alignment index's format, from its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexKind {
    Bai,
    Csi,
    Crai,
}

/// A BGZF virtual offset: the compressed offset of a block, and an offset into its inflated bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VirtualOffset(pub u64);

impl VirtualOffset {
    pub(crate) fn compressed(self) -> u64 {
        self.0 >> 16
    }
    pub(crate) fn uncompressed(self) -> u16 {
        (self.0 & 0xffff) as u16
    }
}

/// Where an index says the reads begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FirstData {
    /// The contig's index in the file's own header order.
    pub(crate) reference: usize,
    /// The first read's 0-based start, when the index records it (a CRAI does). A BAI or CSI does
    /// not: its linear index is too coarse to tell (htslib back-fills a contig's empty leading
    /// windows with the first read's offset, so every window before it looks occupied), so the
    /// start is read from the record at `record`.
    pub(crate) start: Option<u64>,
    /// The first read's virtual offset in the BAM (BAI/CSI), to read its exact start from.
    pub(crate) record: Option<VirtualOffset>,
}

/// The first contig with mapped reads in the index `bytes`. `None` when it has none or is not one.
pub(crate) fn first_data(kind: IndexKind, bytes: &[u8]) -> Option<FirstData> {
    match kind {
        IndexKind::Bai => binning_first(&mut Fields(bytes), false).ok().flatten(),
        IndexKind::Csi => {
            let inflated = MultiGzDecoder::new(bytes).take(MAX_INFLATED_INDEX_BYTES);
            binning_first(&mut Fields(inflated), true).ok().flatten()
        }
        IndexKind::Crai => crai_first(bytes),
    }
}

/// Little-endian fields read from a stream. Every read is of a fixed small size, so a count the
/// file claims bounds only how long the loop over it runs, which the stream's end cuts short.
struct Fields<R>(R);

impl<R: Read> Fields<R> {
    fn bytes<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut buf = [0u8; N];
        self.0.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn u8(&mut self) -> io::Result<u8> {
        self.bytes::<1>().map(|[b]| b)
    }
    fn i32(&mut self) -> io::Result<i32> {
        self.bytes().map(i32::from_le_bytes)
    }
    fn u32(&mut self) -> io::Result<u32> {
        self.bytes().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> io::Result<u64> {
        self.bytes().map(u64::from_le_bytes)
    }
    /// A count, which must not be negative.
    fn count(&mut self) -> io::Result<u64> {
        u64::try_from(self.i32()?).map_err(|_| invalid("a negative count"))
    }
    /// Skip `n` bytes, failing when the stream ends first.
    fn skip(&mut self, n: u64) -> io::Result<()> {
        let skipped = io::copy(&mut (&mut self.0).take(n), &mut io::sink())?;
        if skipped == n {
            Ok(())
        } else {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }
    /// A CRAM ITF8 integer (1-5 bytes, big-endian, its length in the first byte's high bits).
    fn itf8(&mut self) -> io::Result<i32> {
        let b0 = self.u8()?;
        let extra = b0.leading_ones().min(4);
        let mask = if extra == 4 { 0x0f } else { 0xff >> (extra + 1) };
        let mut value = u32::from(b0 & mask);
        for i in 0..extra {
            let b = u32::from(self.u8()?);
            // The fifth byte holds only its low 4 bits.
            value = if i == 3 { (value << 4) | (b & 0x0f) } else { (value << 8) | b };
        }
        Ok(value as i32)
    }
    /// Skip a CRAM LTF8 integer (1-9 bytes, its length in the first byte's high bits).
    fn skip_ltf8(&mut self) -> io::Result<()> {
        let b0 = self.u8()?;
        self.skip(u64::from(b0.leading_ones()))
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// The first contig with mapped reads of a BAI (`csi == false`) or an inflated CSI. A contig's
/// pseudo-bin (37450 at the usual depth) holds its first read's offset and its mapped-read count;
/// without one, its earliest chunk start is where its reads begin.
fn binning_first<R: Read>(f: &mut Fields<R>, csi: bool) -> io::Result<Option<FirstData>> {
    let magic: [u8; 4] = f.bytes()?;
    if magic != if csi { *b"CSI\x01" } else { *b"BAI\x01" } {
        return Err(invalid("not an index"));
    }
    let pseudo_bin = if csi {
        let _min_shift = f.i32()?;
        let depth = f.i32()?;
        // ((1 << ((depth + 1) * 3)) - 1) / 7 + 1, for the depths whose bin numbers fit a u32.
        if !(0..=MAX_CSI_DEPTH).contains(&depth) {
            return Err(invalid("an unsupported CSI depth"));
        }
        let aux = f.count()?;
        f.skip(aux)?;
        u32::try_from(((1u64 << ((depth as u64 + 1) * 3)) - 1) / 7 + 1)
            .map_err(|_| invalid("an unsupported CSI depth"))?
    } else {
        37450
    };
    let n_ref = f.count()?;
    for reference in 0..n_ref {
        // Per contig: its bins (the pseudo-bin among them), then (BAI only) its linear index.
        let mut earliest: Option<u64> = None;
        let mut pseudo: Option<(u64, u64)> = None;
        for _ in 0..f.count()? {
            let bin = f.u32()?;
            if csi {
                let _loffset = f.u64()?;
            }
            let n_chunk = f.count()?;
            if bin == pseudo_bin && n_chunk == 2 {
                let (ref_beg, _ref_end) = (f.u64()?, f.u64()?);
                let (mapped, _unmapped) = (f.u64()?, f.u64()?);
                pseudo = Some((ref_beg, mapped));
                continue;
            }
            for _ in 0..n_chunk {
                let (beg, _end) = (f.u64()?, f.u64()?);
                earliest = Some(earliest.map_or(beg, |e| e.min(beg)));
            }
        }
        let record = match pseudo {
            Some((_, 0)) => None,
            Some((ref_beg, _)) => Some(ref_beg),
            None => earliest,
        };
        if let Some(offset) = record {
            return Ok(Some(FirstData {
                reference: usize::try_from(reference).map_err(|_| invalid("too many contigs"))?,
                start: None,
                record: Some(VirtualOffset(offset)),
            }));
        }
        if !csi {
            // The linear index (8 bytes a window) says nothing the record does not.
            let n_intv = f.count()?;
            f.skip(n_intv.checked_mul(8).ok_or_else(|| invalid("a long linear index"))?)?;
        }
    }
    Ok(None)
}

/// The first slice with mapped reads in a CRAI: gzipped lines of `seq_id`, `alignment_start`
/// (1-based), span, container offset, slice offset, slice size. An unmapped slice has `seq_id` -1, a
/// multi-reference one -2; a coordinate-sorted CRAM's slices come in contig order, so the first
/// single-contig line is the first contig's first slice (unless the CRAM packs its first contigs
/// into multi-reference slices, whose contigs the CRAI does not name: then a later contig's).
fn crai_first(bytes: &[u8]) -> Option<FirstData> {
    let mut lines = io::BufReader::new(MultiGzDecoder::new(bytes).take(MAX_INFLATED_INDEX_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut lines).take(MAX_CRAI_LINE).read_until(b'\n', &mut line).ok()?;
        if n == 0 || (n as u64 == MAX_CRAI_LINE && line.last() != Some(&b'\n')) {
            return None;
        }
        let text = std::str::from_utf8(&line).ok()?;
        let mut fields = text.split('\t').map(str::trim);
        let seq: i64 = fields.next()?.parse().ok()?;
        let start: u64 = fields.next()?.parse().ok()?;
        if let Ok(reference) = usize::try_from(seq) {
            return Some(FirstData {
                reference,
                start: Some(start.saturating_sub(1)),
                record: None,
            });
        }
    }
}

/// What a header read found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeaderName {
    /// The contig's name.
    Name(String),
    /// The bytes end inside the header (or are cut mid-block): read more.
    NeedMore,
    /// Not a BAM (or CRAM), corrupt, inflating past the cap, or the header has no such contig.
    Invalid,
}

/// How a header read that failed is classed: a field that cannot be (a negative count) or an
/// inflate error on corrupt bytes (flate2 reports a bad deflate stream as `InvalidInput`) is
/// invalid; a read cut short (or a block cut mid-way) is a header that goes on past these bytes,
/// unless the inflation cap is what cut it.
fn header_error(e: &io::Error, inflation_capped: bool) -> HeaderName {
    match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => HeaderName::Invalid,
        _ if inflation_capped => HeaderName::Invalid,
        _ => HeaderName::NeedMore,
    }
}

/// The name of contig `reference` in the BAM whose first bytes are `bytes`: inflate the BGZF blocks
/// as a stream (at most [`MAX_INFLATED_HEADER_BYTES`]), skip the header text, and walk the
/// reference list to the one wanted.
pub(crate) fn bam_reference_name(bytes: &[u8], reference: usize) -> HeaderName {
    if !bytes.starts_with(&[0x1f, 0x8b]) && bytes.len() >= 2 {
        return HeaderName::Invalid;
    }
    let mut f = Fields(MultiGzDecoder::new(bytes).take(MAX_INFLATED_HEADER_BYTES));
    let read = |f: &mut Fields<io::Take<MultiGzDecoder<&[u8]>>>| -> io::Result<Option<String>> {
        if f.bytes::<4>()? != *b"BAM\x01" {
            return Ok(None);
        }
        let l_text = f.count()?;
        f.skip(l_text)?;
        let n_ref = f.count()?;
        if reference as u64 >= n_ref {
            return Ok(None);
        }
        for i in 0..=reference {
            let l_name = usize::try_from(f.count()?).map_err(|_| invalid("a long name"))?;
            if l_name > MAX_NAME_BYTES {
                return Ok(None);
            }
            if i < reference {
                f.skip(l_name as u64 + 4)?;
                continue;
            }
            let mut name = vec![0u8; l_name];
            f.0.read_exact(&mut name)?;
            // NUL-terminated.
            let name = name.split(|b| *b == 0).next().unwrap_or_default();
            return Ok(Some(String::from_utf8_lossy(name).into_owned()));
        }
        Ok(None)
    };
    match read(&mut f) {
        Ok(Some(name)) if !name.is_empty() => HeaderName::Name(name),
        Ok(_) => HeaderName::Invalid,
        Err(e) => header_error(&e, f.0.limit() == 0),
    }
}

/// The name of contig `reference` in the CRAM (2.x or 3.x) whose first bytes are `bytes`: the
/// file definition (26 bytes), the header container's header, then its first block, the SAM header
/// (raw or gzip, as the spec allows), whose `@SQ` lines in order are the contigs.
pub(crate) fn cram_reference_name(bytes: &[u8], reference: usize) -> HeaderName {
    let mut f = Fields(bytes);
    let block = |f: &mut Fields<&[u8]>| -> io::Result<Option<(u8, usize)>> {
        let magic: [u8; 4] = f.bytes()?;
        let [major, _minor] = f.bytes::<2>()?;
        if magic != *b"CRAM" || !(2..=3).contains(&major) {
            return Ok(None);
        }
        let _file_id: [u8; 20] = f.bytes()?;
        // The container header: its length, then the reference, start, span and record count
        // (ITF8), the record counter and bases (LTF8), the block count and the landmarks (ITF8),
        // and from CRAM 3 a CRC32.
        let _length = f.i32()?;
        for _ in 0..4 {
            f.itf8()?;
        }
        f.skip_ltf8()?;
        f.skip_ltf8()?;
        f.itf8()?;
        for _ in 0..u64::try_from(f.itf8()?).map_err(|_| invalid("a negative count"))? {
            f.itf8()?;
        }
        if major >= 3 {
            f.skip(4)?;
        }
        // The first block: method, content type (0: the file header), content id, sizes.
        let method = f.u8()?;
        if f.u8()? != 0 {
            return Ok(None);
        }
        f.itf8()?;
        let size = usize::try_from(f.itf8()?).map_err(|_| invalid("a negative size"))?;
        let _raw_size = f.itf8()?;
        Ok(Some((method, size)))
    };
    let (method, size) = match block(&mut f) {
        Ok(Some(found)) => found,
        Ok(None) => return HeaderName::Invalid,
        Err(e) => return header_error(&e, false),
    };
    // The block's bytes must all be here before they are read (a gzip cut short is not corrupt).
    let Some(data) = f.0.get(..size) else {
        return HeaderName::NeedMore;
    };
    let found = match method {
        0 => sam_header_name(data, reference),
        1 => sam_header_name(MultiGzDecoder::new(data).take(MAX_INFLATED_HEADER_BYTES), reference),
        // Other codecs (rANS, bzip2, lzma) are not read: no locus.
        _ => return HeaderName::Invalid,
    };
    match found {
        Ok(Some(name)) if !name.is_empty() => HeaderName::Name(name),
        // Every byte of the block was here: whatever ends it short is invalid.
        _ => HeaderName::Invalid,
    }
}

/// The `SN` of the `reference`-th `@SQ` line of a CRAM's header block (`l_text` then the text).
fn sam_header_name<R: Read>(block: R, reference: usize) -> io::Result<Option<String>> {
    let mut f = Fields(block);
    let l_text = f.count()?;
    let mut text = io::BufReader::new(f.0.take(l_text));
    let mut line = Vec::new();
    let mut seen = 0usize;
    loop {
        line.clear();
        let n = (&mut text).take(MAX_SAM_HEADER_LINE).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if n as u64 == MAX_SAM_HEADER_LINE && line.last() != Some(&b'\n') {
            // An `@SQ` line this long cannot be counted past without its name: every later
            // contig's index would shift by one, so no locus rather than the wrong one.
            if line.starts_with(b"@SQ\t") {
                return Ok(None);
            }
            // Any other long line: skip the rest of it, unkept.
            let mut rest = Vec::new();
            loop {
                rest.clear();
                let m = (&mut text).take(MAX_SAM_HEADER_LINE).read_until(b'\n', &mut rest)?;
                if m == 0 || rest.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        let Some(fields) = line.strip_prefix(b"@SQ\t") else {
            continue;
        };
        if seen == reference {
            let name = fields
                .split(|b| *b == b'\t' || *b == b'\n' || *b == b'\r')
                .find_map(|field| field.strip_prefix(b"SN:"));
            return Ok(name.map(|n| String::from_utf8_lossy(n).into_owned()));
        }
        seen += 1;
    }
}

/// The 0-based start of the BAM record `uncompressed` bytes into the first BGZF block of `bytes`
/// (bytes read from that block's start), when it is a placed record on `reference`. Only the
/// record's first 12 bytes are read.
pub(crate) fn record_start(bytes: &[u8], uncompressed: u16, reference: usize) -> Option<u64> {
    let mut f = Fields(MultiGzDecoder::new(bytes));
    f.skip(u64::from(uncompressed)).ok()?;
    let block_size = f.u32().ok()?;
    // The fixed part of a record is 32 bytes.
    if block_size < 32 {
        return None;
    }
    let ref_id = usize::try_from(f.i32().ok()?).ok()?;
    let pos = u64::try_from(f.i32().ok()?).ok()?;
    (ref_id == reference).then_some(pos)
}

/// `name:start-end`, 1-based inclusive, [`LOCUS_WIDTH`] bases from the 0-based `start`.
pub(crate) fn locus(name: &str, start: u64) -> String {
    format!("{name}:{}-{}", start.saturating_add(1), start.saturating_add(LOCUS_WIDTH))
}

/// The alignment file a locus is found for.
pub(crate) struct LocusQuery<'a> {
    pub(crate) alignment_uri: &'a str,
    /// A CRAM (else a BAM).
    pub(crate) cram: bool,
    pub(crate) index_uri: &'a str,
    pub(crate) index_kind: IndexKind,
}

/// The locus to open `q` at, reading its index and header from S3. `None` when the file has no
/// mapped reads, an input is too large or unreadable, or its contig names are unknown;
/// best-effort, so a failed read is logged and gives `None`.
pub(crate) async fn find(cloud: &dyn ObjectStore, q: &LocusQuery<'_>) -> Option<String> {
    let index = read_capped(cloud, q.index_uri, MAX_INDEX_BYTES).await?;
    let kind = q.index_kind;
    let first = blocking(move || first_data(kind, &index)).await??;
    let parse = if q.cram { cram_reference_name } else { bam_reference_name };
    let name = header_name(cloud, q.alignment_uri, first.reference, parse).await?;
    let exact = match first.record {
        Some(vo) if !q.cram => {
            let at = vo.compressed();
            match cloud.get_object_range(q.alignment_uri, at, at + RECORD_READ_BYTES - 1).await {
                Ok(body) => record_start(&body.bytes, vo.uncompressed(), first.reference),
                Err(e) => {
                    log_read_error(q.alignment_uri, &e);
                    None
                }
            }
        }
        _ => None,
    };
    Some(locus(&name, exact.or(first.start).unwrap_or(0)))
}

/// `work` on a blocking thread (it inflates and parses up to the caps above). `None` when the
/// thread could not run it.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(out) => Some(out),
        Err(e) => {
            tracing::warn!(error = %e, "locus: a parse did not finish");
            None
        }
    }
}

/// Contig `reference`'s name from the file's header, read by `parse`: a first read, then one of up
/// to [`MAX_HEADER_BYTES`] when the header goes on past it.
async fn header_name(
    cloud: &dyn ObjectStore,
    uri: &str,
    reference: usize,
    parse: fn(&[u8], usize) -> HeaderName,
) -> Option<String> {
    let mut want = FIRST_HEADER_READ;
    loop {
        let body = match cloud.get_object_range(uri, 0, want - 1).await {
            Ok(body) => body,
            Err(e) => {
                log_read_error(uri, &e);
                return None;
            }
        };
        let whole = body.total_size.map_or((body.bytes.len() as u64) < want, |t| t <= want);
        let bytes = body.bytes;
        match blocking(move || parse(&bytes, reference)).await? {
            HeaderName::Name(name) => return Some(name),
            HeaderName::NeedMore if !whole && want < MAX_HEADER_BYTES => want = MAX_HEADER_BYTES,
            _ => return None,
        }
    }
}

/// The whole object at `uri` when it is at most `cap` bytes.
async fn read_capped(cloud: &dyn ObjectStore, uri: &str, cap: u64) -> Option<Vec<u8>> {
    match crate::object_view::read_capped(cloud, uri, cap).await {
        Ok(None) => {
            tracing::info!(uri, cap, "locus: input too large to read");
            None
        }
        Ok(Some(bytes)) => Some(bytes),
        Err(e) => {
            log_read_error(uri, &e);
            None
        }
    }
}

fn log_read_error(uri: &str, e: &AwsError) {
    if !matches!(e, AwsError::NotFound) {
        tracing::warn!(uri, error = %e, "locus: read failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAM: &[u8] = include_bytes!("../../tests/fixtures/igv/tiny.bam");
    const BAI: &[u8] = include_bytes!("../../tests/fixtures/igv/tiny.bam.bai");
    const CSI: &[u8] = include_bytes!("../../tests/fixtures/igv/tiny.bam.csi");
    const CRAI: &[u8] = include_bytes!("../../tests/fixtures/igv/tiny.cram.crai");

    /// The fixture's first read is on chrB (the second contig) at 9,001 (1-based).
    const FIRST_START: u64 = 9000;

    /// The locus for the fixture BAM through `index`, as [`find`] reads it.
    fn bam_locus(kind: IndexKind, index: &[u8]) -> Option<String> {
        let first = first_data(kind, index)?;
        let HeaderName::Name(name) = bam_reference_name(BAM, first.reference) else {
            return None;
        };
        let start = first
            .record
            .and_then(|vo| {
                let at = usize::try_from(vo.compressed()).ok()?;
                record_start(BAM.get(at..)?, vo.uncompressed(), first.reference)
            })
            .or(first.start)?;
        Some(locus(&name, start))
    }

    #[test]
    fn the_first_contig_with_reads_and_its_first_read_come_from_the_index() {
        assert_eq!(bam_locus(IndexKind::Bai, BAI).as_deref(), Some("chrB:9001-11000"));
        assert_eq!(bam_locus(IndexKind::Csi, CSI).as_deref(), Some("chrB:9001-11000"));
        let crai = first_data(IndexKind::Crai, CRAI).unwrap();
        assert_eq!((crai.reference, crai.start), (1, Some(FIRST_START)));
        // A BAI or CSI gives no start of its own: only the record's.
        for (kind, index) in [(IndexKind::Bai, BAI), (IndexKind::Csi, CSI)] {
            let first = first_data(kind, index).unwrap();
            assert_eq!((first.reference, first.start), (1, None));
        }
    }

    /// A CRAM's first bytes as samtools writes them: the file definition, the header container's
    /// header, and its first block, the SAM header text (raw, or gzip as samtools writes it), with
    /// an `@SQ` line per name of `contigs`.
    fn cram_header(major: u8, gzip_block: bool, contigs: &[&str]) -> Vec<u8> {
        let mut text = String::from("@HD\tVN:1.6\tSO:coordinate\n");
        text.push_str(&format!("@CO\t{}\n", "x".repeat(70_000)));
        for name in contigs {
            text.push_str(&format!("@SQ\tSN:{name}\tLN:1000\tM5:00\n"));
        }
        let mut payload = (text.len() as i32).to_le_bytes().to_vec();
        payload.extend(text.as_bytes());
        let raw_size = payload.len();
        let data = if gzip_block { gzip(&payload) } else { payload };
        let mut out = b"CRAM".to_vec();
        out.extend([major, 0]);
        out.extend(*b"tiny.cram\0\0\0\0\0\0\0\0\0\0\0");
        out.extend(0i32.to_le_bytes()); // container length (not read)
        out.extend([0, 0, 0, 0]); // reference, start, span, records (ITF8)
        out.extend([0, 0]); // record counter, bases (LTF8)
        out.push(2); // blocks
        out.extend([2, 0, 0x80, 0xf0]); // two landmarks: 0, 240
        if major >= 3 {
            out.extend([0x69, 0x12, 0x8c, 0xa3]); // CRC32 (not checked)
        }
        out.push(u8::from(gzip_block)); // method
        out.push(0); // content type: the file header
        out.push(0); // content id
        out.extend(itf8(data.len() as u32));
        out.extend(itf8(raw_size as u32));
        out.extend(&data);
        out
    }

    /// `n` as a CRAM ITF8.
    fn itf8(n: u32) -> Vec<u8> {
        match n {
            0..0x80 => vec![n as u8],
            0x80..0x4000 => vec![0x80 | (n >> 8) as u8, n as u8],
            0x4000..0x20_0000 => vec![0xc0 | (n >> 16) as u8, (n >> 8) as u8, n as u8],
            0x20_0000..0x1000_0000 => {
                vec![0xe0 | (n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8]
            }
            _ => vec![
                0xf0 | (n >> 28) as u8,
                (n >> 20) as u8,
                (n >> 12) as u8,
                (n >> 4) as u8,
                (n & 0x0f) as u8,
            ],
        }
    }

    #[test]
    fn itf8_reads_every_length() {
        for n in [
            0u32,
            1,
            0x7f,
            0x80,
            0x3fff,
            0x4000,
            0x1f_ffff,
            0x20_0000,
            0x0fff_ffff,
            0x1234_5678,
            u32::MAX,
        ] {
            let bytes = itf8(n);
            assert_eq!(Fields(bytes.as_slice()).itf8().unwrap() as u32, n, "{n:#x}");
        }
        assert!(Fields(&[0x80u8][..]).itf8().is_err());
    }

    #[test]
    fn a_cram_s_contig_is_named_by_its_own_header() {
        // The CRAI's contig 1, in the header's order (whatever order a FASTA lists them in).
        for major in [2, 3] {
            for gzip_block in [false, true] {
                let cram = cram_header(major, gzip_block, &["chrA", "chrB"]);
                assert_eq!(
                    cram_reference_name(&cram, 1),
                    HeaderName::Name("chrB".into()),
                    "CRAM {major}, gzip {gzip_block}"
                );
                let reordered = cram_header(major, gzip_block, &["chrB", "chrA"]);
                assert_eq!(cram_reference_name(&reordered, 1), HeaderName::Name("chrA".into()));
                assert_eq!(cram_reference_name(&cram, 2), HeaderName::Invalid);
                // Cut inside the block: read more.
                assert_eq!(cram_reference_name(&cram[..cram.len() - 10], 1), HeaderName::NeedMore);
                assert_eq!(cram_reference_name(&cram[..30], 1), HeaderName::NeedMore);
            }
        }
        assert_eq!(cram_reference_name(b"", 0), HeaderName::NeedMore);
        assert_eq!(cram_reference_name(BAM, 0), HeaderName::Invalid);
        // A CRAM 4, or a header block in a codec not read (rANS): no name.
        let mut v4 = cram_header(3, false, &["chrA"]);
        v4[4] = 4;
        assert_eq!(cram_reference_name(&v4, 0), HeaderName::Invalid);
        let mut rans = cram_header(3, false, &["chrA"]);
        let method = rans.iter().position(|b| *b == 0xa3).unwrap() + 1;
        rans[method] = 4;
        assert_eq!(cram_reference_name(&rans, 0), HeaderName::Invalid);
        // Cut or bent anywhere: an answer, never a panic.
        let cram = cram_header(3, true, &["chrA", "chrB"]);
        for cut in 0..cram.len().min(2000) {
            let _ = cram_reference_name(&cram[..cut], 1);
        }
        for i in 0..cram.len().min(2000) {
            let mut bent = cram.clone();
            bent[i] ^= 0x5a;
            let _ = cram_reference_name(&bent, 1);
        }
    }

    #[test]
    fn a_header_cut_short_asks_for_more_and_garbage_is_invalid() {
        assert_eq!(bam_reference_name(BAM, 0), HeaderName::Name("chrA".into()));
        assert_eq!(bam_reference_name(BAM, 1), HeaderName::Name("chrB".into()));
        assert_eq!(bam_reference_name(BAM, 2), HeaderName::Invalid);
        assert_eq!(bam_reference_name(&BAM[..20], 1), HeaderName::NeedMore);
        assert_eq!(bam_reference_name(&[], 1), HeaderName::NeedMore);
        assert_eq!(bam_reference_name(b"not a bam at all, just text", 0), HeaderName::Invalid);
        // A gzip of something else.
        assert_eq!(bam_reference_name(&gzip(b"BAI\x01 and more bytes"), 0), HeaderName::Invalid);
    }

    #[test]
    fn an_sq_line_too_long_to_read_gives_no_locus_rather_than_the_next_contig_s() {
        let block = |lines: &[Vec<u8>]| {
            let text: Vec<u8> = lines.concat();
            let mut b = (text.len() as i32).to_le_bytes().to_vec();
            b.extend(text);
            b
        };
        let sq = |name: &str| format!("@SQ\tSN:{name}\tLN:5\n").into_bytes();
        let mut long_sq = b"@SQ\tSN:chrLong\t".to_vec();
        long_sq.extend(vec![b'x'; MAX_SAM_HEADER_LINE as usize]);
        long_sq.push(b'\n');
        let mut long_co = b"@CO\t".to_vec();
        long_co.extend(vec![b'x'; MAX_SAM_HEADER_LINE as usize]);
        long_co.push(b'\n');
        // The second contig is past an overlong @SQ: it cannot be told which it is.
        let data = block(&[sq("chr1"), long_sq, sq("chr3")]);
        assert_eq!(sam_header_name(data.as_slice(), 2).unwrap(), None);
        // A long line of another kind is skipped, uncounted.
        let data = block(&[sq("chr1"), long_co, sq("chr2")]);
        assert_eq!(sam_header_name(data.as_slice(), 1).unwrap(), Some("chr2".to_string()));
    }

    #[test]
    fn a_long_header_is_read_past_its_text() {
        // l_text of 200 kB, then two references: the name is found past the text.
        let mut raw = b"BAM\x01".to_vec();
        let text = vec![b'@'; 200_000];
        raw.extend((text.len() as i32).to_le_bytes());
        raw.extend(&text);
        raw.extend(2i32.to_le_bytes());
        for (name, len) in [("chr1", 100i32), ("chrUn_long_name", 5)] {
            raw.extend((name.len() as i32 + 1).to_le_bytes());
            raw.extend(name.as_bytes());
            raw.push(0);
            raw.extend(len.to_le_bytes());
        }
        let bgzf = gzip(&raw);
        assert_eq!(bam_reference_name(&bgzf, 1), HeaderName::Name("chrUn_long_name".into()));
        assert_eq!(bam_reference_name(&bgzf[..bgzf.len() / 2], 1), HeaderName::NeedMore);
        // A name length past the cap is refused, not allocated.
        let mut huge = b"BAM\x01".to_vec();
        huge.extend(0i32.to_le_bytes());
        huge.extend(1i32.to_le_bytes());
        huge.extend(i32::MAX.to_le_bytes());
        assert_eq!(bam_reference_name(&gzip(&huge), 0), HeaderName::Invalid);
        // So is a negative count.
        let mut negative = b"BAM\x01".to_vec();
        negative.extend((-5i32).to_le_bytes());
        assert_eq!(bam_reference_name(&gzip(&negative), 0), HeaderName::Invalid);
    }

    #[test]
    fn a_header_that_inflates_past_the_cap_is_invalid_and_stops_there() {
        // l_text = i32::MAX over a gzip of zeros: the skip stops at the inflation cap, and the
        // answer is final (not "read more").
        let mut raw = b"BAM\x01".to_vec();
        raw.extend(i32::MAX.to_le_bytes());
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &raw).unwrap();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..(MAX_INFLATED_HEADER_BYTES >> 20) + 1 {
            std::io::Write::write_all(&mut enc, &zeros).unwrap();
        }
        let bomb = enc.finish().unwrap();
        assert!(bomb.len() < 1 << 20, "{}", bomb.len());
        assert_eq!(bam_reference_name(&bomb, 0), HeaderName::Invalid);
    }

    #[test]
    fn corrupt_bgzf_is_invalid_not_a_reason_to_read_more() {
        let header_len = BAM.len().min(64);
        for i in 20..header_len {
            let mut bent = BAM.to_vec();
            bent[i] ^= 0xff;
            assert_ne!(bam_reference_name(&bent, 1), HeaderName::NeedMore, "byte {i} flipped");
        }
    }

    #[test]
    fn a_csi_depth_whose_bins_do_not_fit_32_bits_is_refused() {
        let csi = |depth: i32| {
            let mut raw = b"CSI\x01".to_vec();
            raw.extend(14i32.to_le_bytes());
            raw.extend(depth.to_le_bytes());
            raw.extend(0i32.to_le_bytes()); // l_aux
            raw.extend(0i32.to_le_bytes()); // n_ref
            gzip(&raw)
        };
        let deepest = csi(MAX_CSI_DEPTH);
        let mut f = Fields(MultiGzDecoder::new(deepest.as_slice()));
        assert!(matches!(binning_first(&mut f, true), Ok(None)));
        for depth in [MAX_CSI_DEPTH + 1, 20, -1] {
            let too_deep = csi(depth);
            let mut f = Fields(MultiGzDecoder::new(too_deep.as_slice()));
            assert!(binning_first(&mut f, true).is_err(), "depth {depth}");
        }
    }

    #[test]
    fn truncated_or_corrupt_inputs_give_none_never_a_panic() {
        for (kind, index) in [(IndexKind::Bai, BAI), (IndexKind::Csi, CSI), (IndexKind::Crai, CRAI)]
        {
            for cut in 0..index.len() {
                let _ = first_data(kind, &index[..cut]);
            }
            for i in 0..index.len() {
                for bit in [0x01, 0x80, 0xff] {
                    let mut bent = index.to_vec();
                    bent[i] ^= bit;
                    let _ = first_data(kind, &bent);
                }
            }
        }
        // Cut where it matters, the whole index is needed for the answer.
        assert_eq!(first_data(IndexKind::Bai, &BAI[..BAI.len() / 2]), None);
        assert_eq!(first_data(IndexKind::Bai, b"BAI\x01"), None);
        assert_eq!(first_data(IndexKind::Bai, b"junk"), None);
        assert_eq!(first_data(IndexKind::Csi, b"junk"), None);
        assert_eq!(first_data(IndexKind::Crai, b"junk"), None);
        for cut in 0..BAM.len() {
            let _ = bam_reference_name(&BAM[..cut], 1);
            let _ = record_start(&BAM[..cut], 0, 1);
        }
        for i in 0..BAM.len() {
            let mut bent = BAM.to_vec();
            bent[i] ^= 0x5a;
            let _ = bam_reference_name(&bent, 1);
            let _ = record_start(&bent, 10, 1);
        }
        assert_eq!(record_start(&[], 0, 0), None);
        assert_eq!(record_start(BAM, u16::MAX, 1), None);
        // A record on another contig is not this contig's start.
        let first = first_data(IndexKind::Bai, BAI).unwrap().record.unwrap();
        let at = first.compressed() as usize;
        assert_eq!(record_start(&BAM[at..], first.uncompressed(), 1), Some(FIRST_START));
        assert_eq!(record_start(&BAM[at..], first.uncompressed(), 0), None);
    }

    #[test]
    fn an_index_with_no_mapped_reads_has_no_locus() {
        // A BAI with two contigs and no bins: magic, n_ref = 2, each n_bin = 0, n_intv = 0.
        let mut empty = b"BAI\x01".to_vec();
        empty.extend(2i32.to_le_bytes());
        for _ in 0..2 {
            empty.extend(0i32.to_le_bytes());
            empty.extend(0i32.to_le_bytes());
        }
        assert_eq!(first_data(IndexKind::Bai, &empty), None);
        // A contig whose pseudo-bin counts only unmapped (placed) reads is skipped.
        let mut placed = b"BAI\x01".to_vec();
        placed.extend(1i32.to_le_bytes());
        placed.extend(2i32.to_le_bytes()); // two bins: a real one and the pseudo-bin
        placed.extend(4681u32.to_le_bytes());
        placed.extend(1i32.to_le_bytes());
        placed.extend(100u64.to_le_bytes());
        placed.extend(200u64.to_le_bytes());
        placed.extend(37450u32.to_le_bytes());
        placed.extend(2i32.to_le_bytes());
        for v in [100u64, 200, 0, 7] {
            placed.extend(v.to_le_bytes());
        }
        placed.extend(0i32.to_le_bytes());
        assert_eq!(first_data(IndexKind::Bai, &placed), None);
        // Without a pseudo-bin, its earliest chunk is where its reads begin.
        let mut bare = b"BAI\x01".to_vec();
        bare.extend(1i32.to_le_bytes());
        bare.extend(1i32.to_le_bytes());
        bare.extend(4681u32.to_le_bytes());
        bare.extend(2i32.to_le_bytes());
        for v in [900u64 << 16, 950 << 16, (500 << 16) | 3, 600 << 16] {
            bare.extend(v.to_le_bytes());
        }
        bare.extend(0i32.to_le_bytes());
        let first = first_data(IndexKind::Bai, &bare).unwrap();
        assert_eq!(first.record.map(|v| (v.compressed(), v.uncompressed())), Some((500, 3)));
        // A CRAI of unmapped and multi-reference slices only.
        let unmapped = gzip(b"-1\t0\t0\t10\t20\t30\n-2\t0\t0\t40\t20\t30\n");
        assert_eq!(first_data(IndexKind::Crai, &unmapped), None);
        let later = gzip(b"-2\t0\t0\t40\t20\t30\n3\t120\t50\t40\t20\t30\n");
        assert_eq!(
            first_data(IndexKind::Crai, &later).map(|f| (f.reference, f.start)),
            Some((3, Some(119)))
        );
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn a_locus_is_a_2_kb_window_from_the_start() {
        assert_eq!(locus("chr1", 0), "chr1:1-2000");
        assert_eq!(locus("chrB", 9000), "chrB:9001-11000");
        assert_eq!(locus("x", u64::MAX), format!("x:{}-{}", u64::MAX, u64::MAX));
    }
}
