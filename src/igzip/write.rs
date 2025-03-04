//! Encoder and Decoder implementing `std::io::Write`
use crate::igzip::*;
use std::io;
use std::io::Write;

/// Streaming compression for input streams implementing `std::io::Write`.
///
/// Notes
/// -----
/// One should consider using `crate::igzip::compress` or `crate::igzip::compress_into` if possible.
/// In that context, we do not need to hold and maintain intermediate buffers for reading and writing.
///
/// Example
/// -------
/// ```
/// use std::{io, io::Write};
/// use isal::igzip::{write::Encoder, CompressionLevel, decompress, Codec};
///
/// let data = b"Hello, World!".to_vec();
/// let mut compressed = vec![];
///
/// let mut encoder = Encoder::new(&mut compressed, CompressionLevel::Three, Codec::Gzip);
///
/// // Numbeer of compressed bytes written to `output`
/// io::copy(&mut io::Cursor::new(&data), &mut encoder).unwrap();
///
/// // call .flush to finish the stream
/// encoder.flush().unwrap();
///
/// let decompressed = decompress(io::Cursor::new(&compressed), Codec::Gzip).unwrap();
/// assert_eq!(decompressed.as_slice(), data);
///
/// ```
pub struct Encoder<W: io::Write> {
    inner: W,
    stream: ZStream,
    out_buf: Vec<u8>,
    dsts: usize,
    dste: usize,
    total_in: usize,
    total_out: usize,
    codec: Codec,
}

impl<W: io::Write> Encoder<W> {
    /// Create a new `Encoder` which implements the `std::io::Read` trait.
    pub fn new(writer: W, level: CompressionLevel, codec: Codec) -> Encoder<W> {
        let out_buf = Vec::with_capacity(BUF_SIZE);

        let mut zstream = ZStream::new(level, ZStreamKind::Stateful);

        zstream.stream.end_of_stream = 0;
        zstream.stream.flush = FlushFlags::NoFlush as _;
        zstream.stream.gzip_flag = codec as _;

        Self {
            inner: writer,
            stream: zstream,
            out_buf,
            dste: 0,
            dsts: 0,
            total_in: 0,
            total_out: 0,
            codec,
        }
    }

    /// Mutable reference to underlying reader, not advisable to modify during reading.
    pub fn get_ref_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    // Reference to underlying reader
    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    #[inline(always)]
    fn write_from_out_buf(&mut self) -> io::Result<usize> {
        let count = self.dste - self.dsts;
        self.inner
            .write_all(&mut self.out_buf[self.dsts..self.dste])?;
        self.out_buf.truncate(0);
        self.dsts = 0;
        self.dste = 0;
        Ok(count)
    }

    /// Call flush and return the inner writer
    pub fn finish(mut self) -> io::Result<W> {
        self.flush()?;
        Ok(self.inner)
    }

    /// total bytes written to the writer, inclusive of all streams if `flush` has been called before
    pub fn total_out(&self) -> usize {
        self.stream.stream.total_out as usize + self.total_out
    }

    /// total bytes processed, inclusive of all streams if `flush` has been called before
    pub fn total_in(&self) -> usize {
        self.stream.stream.total_in as usize + self.total_in
    }
}

impl<W: io::Write> io::Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.stream.stream.avail_in = buf.len() as _;
        self.stream.stream.next_in = buf.as_ptr() as *mut _;

        while self.stream.stream.avail_in > 0 {
            self.out_buf.resize(self.dste + BUF_SIZE, 0);

            self.stream.stream.avail_out = BUF_SIZE as _;
            self.stream.stream.next_out =
                self.out_buf[self.dste..self.dste + BUF_SIZE].as_mut_ptr();

            self.stream.deflate()?;

            self.dste += BUF_SIZE - self.stream.stream.avail_out as usize;
        }

        self.write_from_out_buf()?;

        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // Write footer and flush to inner
        self.stream.stream.end_of_stream = 1;
        self.stream.stream.flush = FlushFlags::FullFlush as _;
        while self.stream.stream.internal_state.state != isal::isal_zstate_state_ZSTATE_END {
            self.out_buf.resize(self.dste + BUF_SIZE, 0);
            self.stream.stream.avail_out = BUF_SIZE as _;
            self.stream.stream.next_out =
                self.out_buf[self.dste..self.dste + BUF_SIZE].as_mut_ptr();
            self.stream.deflate()?;
            self.dste += BUF_SIZE - self.stream.stream.avail_out as usize;
        }
        self.write_from_out_buf()?;
        self.inner.flush()?;

        // Prep for next stream should user call 'write' again after flush.
        // needs to store total_in/out separately as checksum is calculated
        // from these values per stream
        self.total_in += self.stream.stream.total_in as usize;
        self.total_out += self.stream.stream.total_out as usize;
        unsafe { isal::isal_deflate_reset(&mut self.stream.stream) };

        self.stream.stream.flush = FlushFlags::NoFlush as _;
        self.stream.stream.end_of_stream = 0;
        self.stream.stream.gzip_flag = self.codec as _;
        Ok(())
    }
}

/// Streaming compression for input streams implementing `std::io::Write`.
///
/// Notes
/// -----
/// One should consider using `crate::igzip::decompress` or `crate::igzip::decompress_into` if possible.
/// In that context, we do not need to hold and maintain intermediate buffers for reading and writing.
///
/// Example
/// -------
/// ```
/// use std::{io, io::Write};
/// use isal::igzip::{write::Decoder, CompressionLevel, compress, Codec};
/// let data = b"Hello, World!".to_vec();
///
/// let compressed = compress(io::Cursor::new(data.as_slice()), CompressionLevel::Three, Codec::Gzip).unwrap();
///
/// let mut decompressed = vec![];
/// let mut decoder = Decoder::new(&mut decompressed, Codec::Gzip);
///
/// // Numbeer of compressed bytes written to `output`
/// let n = io::copy(&mut io::Cursor::new(&compressed), &mut decoder).unwrap();
/// assert_eq!(n as usize, compressed.len());
/// assert_eq!(decompressed.as_slice(), data);
/// ```
pub struct Decoder<W: io::Write> {
    inner: W,
    zst: InflateState,
    out_buf: Vec<u8>,
    dsts: usize,
    dste: usize,
    codec: Codec,
    adler32: u32,
}

impl<W: io::Write> Decoder<W> {
    pub fn new(writer: W, codec: Codec) -> Decoder<W> {
        let mut zst = InflateState::new();
        zst.0.crc_flag = codec as _;

        Self {
            inner: writer,
            zst,
            out_buf: Vec::with_capacity(BUF_SIZE),
            dste: 0,
            dsts: 0,
            codec,
            adler32: 1,
        }
    }

    /// Mutable reference to underlying reader, not advisable to modify during reading.
    pub fn get_ref_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    // Reference to underlying reader
    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    #[inline(always)]
    fn write_from_out_buf(&mut self) -> io::Result<usize> {
        let count = self.dste - self.dsts;
        self.inner
            .write_all(&mut self.out_buf[self.dsts..self.dste])?;
        self.out_buf.truncate(0);
        self.dsts = 0;
        self.dste = 0;
        Ok(count)
    }
}

impl<W: io::Write> io::Write for Decoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Check if there is data left in out_buf, otherwise refill; if end state, return 0
        // Read out next buf len worth to compress; filling intermediate out_buf
        debug_assert_eq!(self.zst.0.avail_in, 0);
        self.zst.0.avail_in = buf.len() as _;
        self.zst.0.next_in = buf.as_ptr() as *mut _;

        let mut n_bytes = 0;
        while self.zst.0.avail_in > 0 {
            if self.zst.block_state() == isal::isal_block_state_ISAL_BLOCK_NEW_HDR {
                // Read gzip header
                if self.codec == Codec::Gzip {
                    // Read this member's gzip header
                    let mut gz_hdr: mem::MaybeUninit<isal::isal_gzip_header> =
                        mem::MaybeUninit::uninit();
                    unsafe { isal::isal_gzip_header_init(gz_hdr.as_mut_ptr()) };
                    let mut gz_hdr = unsafe { gz_hdr.assume_init() };
                    read_gzip_header(&mut self.zst.0, &mut gz_hdr)?;

                // Read zlib header
                } else if self.codec == Codec::Zlib {
                    self.zst.0.crc_flag = 0; // zlib uses adler-32

                    let mut hdr: mem::MaybeUninit<isal::isal_zlib_header> =
                        mem::MaybeUninit::uninit();
                    unsafe { isal::isal_zlib_header_init(hdr.as_mut_ptr()) };
                    let mut hdr = unsafe { hdr.assume_init() };
                    read_zlib_header(&mut self.zst.0, &mut hdr)?;
                    self.zst.0.next_in = buf[2..].as_ptr() as *mut _; // skip header now that it's read
                                                                      // self.zst.0.avail_in -= 4; // skip adler-32 trailer
                }
            }

            // decompress member
            loop {
                self.out_buf.resize(n_bytes + BUF_SIZE, 0);

                self.zst.0.next_out = self.out_buf[n_bytes..n_bytes + BUF_SIZE].as_mut_ptr();
                self.zst.0.avail_out = BUF_SIZE as _;

                self.zst.step_inflate()?;

                n_bytes += BUF_SIZE - self.zst.0.avail_out as usize;

                let state = self.zst.block_state();
                match self.codec {
                    Codec::Deflate | Codec::Zlib => {
                        // On block finished we're done done w/ the block,
                        // on block coded, we need to move onto the next input buffer
                        if state == isal::isal_block_state_ISAL_BLOCK_FINISH
                            || state == isal::isal_block_state_ISAL_BLOCK_CODED
                        {
                            break;
                        }
                    }
                    Codec::Gzip => {
                        if state == isal::isal_block_state_ISAL_BLOCK_CODED
                            || state == isal::isal_block_state_ISAL_BLOCK_TYPE0
                            || state == isal::isal_block_state_ISAL_BLOCK_HDR
                            || state == isal::isal_block_state_ISAL_BLOCK_FINISH
                        {
                            break;
                        }
                    }
                }
            }
            if self.zst.0.block_state == isal::isal_block_state_ISAL_BLOCK_FINISH {
                self.zst.reset();
            }
        }
        // zlib adler32
        if self.codec == Codec::Zlib && buf.len() > 4 {
            // Update adler
            self.adler32 =
                unsafe { isal::isal_adler32(self.adler32, self.out_buf.as_ptr(), n_bytes as _) };

            // when end of block, verify adler matches (state reset above on block finish)
            if self.zst.block_state() == isal::isal_block_state_ISAL_BLOCK_NEW_HDR {
                // unwrap ok, ensured buf len > 4 above
                debug_assert!(buf.len() > 4);
                let bytes: [u8; 4] = (buf[buf.len() - 4..buf.len()]).try_into().unwrap();
                let expected_adler32 = u32::from_be_bytes(bytes);
                if self.adler32 != expected_adler32 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        Error::DecompressionError(DecompCode::IncorrectChecksum),
                    ));
                }
            }
        }
        self.out_buf.truncate(n_bytes);
        self.dste = n_bytes;
        self.dsts = 0;
        self.write_from_out_buf()?;

        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        loop {
            if self.write_from_out_buf()? == 0 {
                break;
            }
        }
        self.inner.flush()
    }
}

/// Deflate compression
/// Basically a wrapper to `Encoder` which sets the codec for you.
pub struct DeflateEncoder<R: io::Write> {
    inner: Encoder<R>,
}

impl<W: io::Write> DeflateEncoder<W> {
    pub fn new(writer: W, level: CompressionLevel) -> Self {
        Self {
            inner: Encoder::new(writer, level, Codec::Deflate),
        }
    }
}

impl<W: io::Write> io::Write for DeflateEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Deflate decompression
/// Basically a wrapper to `Decoder` which sets the codec for you.
pub struct DeflateDecoder<W: io::Write> {
    inner: Decoder<W>,
}

impl<W: io::Write> DeflateDecoder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Decoder::new(writer, Codec::Deflate),
        }
    }
}

impl<W: io::Write> io::Write for DeflateDecoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Zlib compression
/// Basically a wrapper to `Encoder` which sets the codec for you.
pub struct ZlibEncoder<R: io::Write> {
    inner: Encoder<R>,
}

impl<W: io::Write> ZlibEncoder<W> {
    pub fn new(writer: W, level: CompressionLevel) -> Self {
        Self {
            inner: Encoder::new(writer, level, Codec::Zlib),
        }
    }
}

impl<W: io::Write> io::Write for ZlibEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Zlib decompression
/// Basically a wrapper to `Decoder` which sets the codec for you.
pub struct ZlibDecoder<W: io::Write> {
    inner: Decoder<W>,
}

impl<W: io::Write> ZlibDecoder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Decoder::new(writer, Codec::Zlib),
        }
    }
}

impl<W: io::Write> io::Write for ZlibDecoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Gzip compression
/// Basically a wrapper to `Encoder` which sets the codec for you.
pub struct GzipEncoder<R: io::Write> {
    inner: Encoder<R>,
}

impl<W: io::Write> GzipEncoder<W> {
    pub fn new(writer: W, level: CompressionLevel) -> Self {
        Self {
            inner: Encoder::new(writer, level, Codec::Gzip),
        }
    }
}

impl<W: io::Write> io::Write for GzipEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Gzip decompression
/// Basically a wrapper to `Decoder` which sets the codec for you.
pub struct GzipDecoder<W: io::Write> {
    inner: Decoder<W>,
}

impl<W: io::Write> GzipDecoder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Decoder::new(writer, Codec::Gzip),
        }
    }
}

impl<W: io::Write> io::Write for GzipDecoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
pub mod tests {
    use io::Cursor;

    use super::*;
    use crate::igzip::tests::{gen_large_data, same_same};
    use std::io::Write;

    #[test]
    fn test_encoder_basic_small() {
        test_encoder_basic(&gen_large_data())
    }
    #[test]
    fn test_encoder_basic_large() {
        test_encoder_basic(&gen_large_data())
    }
    fn test_encoder_basic(data: &[u8]) {
        let mut compressed = vec![];
        let mut encoder = Encoder::new(&mut compressed, CompressionLevel::Three, Codec::Gzip);
        let nbytes = io::copy(&mut io::Cursor::new(&data), &mut encoder).unwrap();

        // Footer isn't written until .flush is called
        let before_flush_bytes_out = encoder.total_out();
        encoder.flush().unwrap();
        let after_flush_bytes_out = encoder.total_out();
        assert!(before_flush_bytes_out < after_flush_bytes_out);

        // nbytes read equals data lenth; compressed is between 0 and data length
        assert_eq!(nbytes, data.len() as _);
        assert!(compressed.len() > 0);
        assert!(compressed.len() < data.len());

        // total out after flush should equal compressed length
        assert_eq!(after_flush_bytes_out, compressed.len());

        // and can be decompressed
        let decompressed =
            crate::igzip::decompress(io::Cursor::new(&compressed), Codec::Gzip).unwrap();
        assert!(same_same(&decompressed, &data));
    }

    #[test]
    fn test_encoder_multi_stream() {
        let first = b"foo";
        let second = b"bar";

        let mut compressed = vec![];
        let mut encoder = Encoder::new(&mut compressed, CompressionLevel::Three, Codec::Gzip);

        encoder.write_all(first).unwrap();
        encoder.flush().unwrap();
        assert_eq!(encoder.total_in(), first.len());

        encoder.write_all(second).unwrap();
        encoder.flush().unwrap();
        assert_eq!(encoder.total_in(), first.len() + second.len());

        let decompressed =
            crate::igzip::decompress(io::Cursor::new(&compressed), Codec::Gzip).unwrap();
        assert_eq!(&decompressed, b"foobar");
    }

    #[test]
    fn test_decoder_basic_small() {
        test_decoder_basic(b"foobar")
    }
    #[test]
    fn test_decoder_basic_large() {
        test_decoder_basic(&gen_large_data())
    }
    fn test_decoder_basic(data: &[u8]) {
        let compressed =
            crate::igzip::compress(io::Cursor::new(&data), CompressionLevel::Three, Codec::Gzip)
                .unwrap();

        let mut decompressed = vec![];
        let mut decoder = Decoder::new(&mut decompressed, Codec::Gzip);
        let nbytes = io::copy(&mut io::Cursor::new(&compressed), &mut decoder).unwrap();
        assert_eq!(nbytes, compressed.len() as u64);
        assert!(same_same(&decompressed, &data));
    }

    #[test]
    fn test_decoder_multi_stream() {
        let first = b"foo";
        let second = b"bar";

        let mut compressed = crate::igzip::compress(
            io::Cursor::new(&first),
            CompressionLevel::Three,
            Codec::Gzip,
        )
        .unwrap();
        compressed.extend(
            crate::igzip::compress(
                io::Cursor::new(&second),
                CompressionLevel::Three,
                Codec::Gzip,
            )
            .unwrap(),
        );

        let mut decompressed = vec![];
        let mut decoder = Decoder::new(&mut decompressed, Codec::Gzip);

        let nbytes = io::copy(&mut io::Cursor::new(&compressed), &mut decoder).unwrap();
        assert_eq!(nbytes, compressed.len() as _);
        assert_eq!(&decompressed, b"foobar");
    }

    #[test]
    fn flate2_gzip_compat_encoder_out_small() {
        flate2_gzip_compat_encoder_out(b"foobar")
    }
    #[test]
    fn flate2_gzip_compat_encoder_out_large() {
        flate2_gzip_compat_encoder_out(&gen_large_data())
    }
    fn flate2_gzip_compat_encoder_out(data: &[u8]) {
        // our encoder
        let mut compressed = vec![];
        {
            let mut encoder = Encoder::new(&mut compressed, CompressionLevel::Three, Codec::Gzip);
            io::copy(&mut Cursor::new(&data), &mut encoder).unwrap();
            encoder.flush().unwrap();
        }

        // their decoder
        let mut decompressed = vec![];
        {
            let mut decoder = flate2::write::GzDecoder::new(&mut decompressed);
            io::copy(&mut Cursor::new(&compressed), &mut decoder).unwrap();
            decoder.flush().unwrap();
        }

        assert!(same_same(&data, &decompressed));
    }

    #[test]
    fn flate2_gzip_compat_decoder_out_small() {
        flate2_gzip_compat_decoder_out(b"foobar");
    }
    #[test]
    fn flate2_gzip_compat_decoder_out_large() {
        flate2_gzip_compat_decoder_out(&gen_large_data());
    }
    fn flate2_gzip_compat_decoder_out(data: &[u8]) {
        // their encoder
        let mut compressed = vec![];
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::fast());
            io::copy(&mut Cursor::new(&data), &mut encoder).unwrap();
            encoder.flush().unwrap();
        }

        // our decoder
        let mut decompressed = vec![];
        {
            let mut decoder = Decoder::new(&mut decompressed, Codec::Gzip);
            io::copy(&mut Cursor::new(&compressed), &mut decoder).unwrap();
            decoder.flush().unwrap();
        }

        assert!(same_same(&data, &decompressed));
    }

    #[test]
    fn flate2_deflate_compat_encoder_out_small() {
        flate2_deflate_compat_encoder_out(b"foobar");
    }
    #[test]
    fn flate2_deflate_compat_encoder_out_large() {
        flate2_deflate_compat_encoder_out(&gen_large_data());
    }
    fn flate2_deflate_compat_encoder_out(data: &[u8]) {
        // our encoder
        let mut compressed = vec![];
        {
            let mut encoder = DeflateEncoder::new(&mut compressed, CompressionLevel::Three);
            io::copy(&mut Cursor::new(&data), &mut encoder).unwrap();
            encoder.flush().unwrap(); // TODO: impl flush on drop
        }

        // their decoder
        let mut decompressed = vec![];
        {
            let mut decoder = flate2::write::DeflateDecoder::new(&mut decompressed);
            io::copy(&mut Cursor::new(&compressed), &mut decoder).unwrap();
            decoder.flush().unwrap();
        }

        assert!(same_same(&data, &decompressed));
    }

    #[test]
    fn flate2_deflate_compat_decoder_out_small() {
        flate2_deflate_compat_decoder_out(b"foobar");
    }
    #[test]
    fn flate2_deflate_compat_decoder_out_large() {
        flate2_deflate_compat_decoder_out(&gen_large_data());
    }
    fn flate2_deflate_compat_decoder_out(data: &[u8]) {
        // their encoder
        let mut compressed = vec![];
        {
            let mut encoder =
                flate2::write::DeflateEncoder::new(&mut compressed, flate2::Compression::fast());
            io::copy(&mut Cursor::new(&data), &mut encoder).unwrap();
            encoder.flush().unwrap();
        }

        // our decoder
        let mut decompressed = vec![];
        {
            let mut decoder = DeflateDecoder::new(&mut decompressed);
            io::copy(&mut Cursor::new(&compressed), &mut decoder).unwrap();
            decoder.flush().unwrap(); // TODO: impl flush on drop
        }
        assert_eq!(data.len(), decompressed.len());
        assert!(same_same(&data, &decompressed));
    }

    #[test]
    fn flate2_zlib_compat_decoder_out_small() {
        flate2_zlib_compat_decoder_out(b"foobar");
    }
    #[test]
    fn flate2_zlib_compat_decoder_out_large() {
        flate2_zlib_compat_decoder_out(&gen_large_data());
    }
    fn flate2_zlib_compat_decoder_out(data: &[u8]) {
        // their encoder
        let mut compressed = vec![];
        {
            let mut encoder =
                flate2::write::ZlibEncoder::new(&mut compressed, flate2::Compression::fast());
            io::copy(&mut Cursor::new(&data), &mut encoder).unwrap();
            encoder.flush().unwrap();
        }

        // our decoder
        let mut decompressed = vec![];
        {
            let mut decoder = ZlibDecoder::new(&mut decompressed);
            io::copy(&mut Cursor::new(&compressed), &mut decoder).unwrap();
            decoder.flush().unwrap(); // TODO: impl flush on drop
        }

        println!(
            "data.len() - decompressed.len() = {}",
            data.len() - decompressed.len()
        );

        assert_eq!(data.len(), decompressed.len());
        assert!(same_same(&data, &decompressed));
    }
}
