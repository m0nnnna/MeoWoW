//! Sector decompression.
//!
//! A compressed sector begins with a one-byte mask naming the algorithms that
//! were applied. Multiple bits may be set; the original packer applied them in
//! a fixed order, so unpacking runs that order in reverse.

use std::io::Read;

use crate::Error;

pub mod mask {
    pub const HUFFMAN: u8 = 0x01;
    pub const ZLIB: u8 = 0x02;
    pub const PKWARE: u8 = 0x08;
    pub const BZIP2: u8 = 0x10;
    pub const SPARSE: u8 = 0x20;
    pub const ADPCM_MONO: u8 = 0x40;
    pub const ADPCM_STEREO: u8 = 0x80;
}

/// Decompresses one sector of a `COMPRESS`-flagged file.
///
/// `expected` is the sector's original size, which the format stores out of
/// band; the compressors themselves are not always self-terminating.
pub fn decompress(data: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let (&flags, body) = data.split_first().ok_or(Error::TruncatedSector)?;

    // Order matters: the packer applies compression last, so we undo it first,
    // then the sample-domain transforms.
    let mut out = if flags & mask::ZLIB != 0 {
        inflate(body, expected)?
    } else if flags & mask::BZIP2 != 0 {
        bunzip2(body, expected)?
    } else if flags & mask::PKWARE != 0 {
        return Err(Error::UnsupportedCompression(mask::PKWARE));
    } else if flags & mask::HUFFMAN != 0 {
        return Err(Error::UnsupportedCompression(mask::HUFFMAN));
    } else if flags == 0 {
        body.to_vec()
    } else {
        return Err(Error::UnsupportedCompression(flags));
    };

    if flags & mask::SPARSE != 0 {
        out = unsparse(&out)?;
    }
    // ADPCM only ever appears on audio, which nothing upstream requests yet.
    if flags & (mask::ADPCM_MONO | mask::ADPCM_STEREO) != 0 {
        return Err(Error::UnsupportedCompression(
            flags & (mask::ADPCM_MONO | mask::ADPCM_STEREO),
        ));
    }

    Ok(out)
}

fn inflate(body: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(expected);
    flate2::read::ZlibDecoder::new(body)
        .read_to_end(&mut out)
        .map_err(|e| Error::Decompress(e.to_string()))?;
    Ok(out)
}

fn bunzip2(body: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(expected);
    bzip2_rs::DecoderReader::new(body)
        .read_to_end(&mut out)
        .map_err(|e| Error::Decompress(e.to_string()))?;
    Ok(out)
}

/// Undoes run-length elimination of zero bytes.
///
/// The stream is a 4-byte big-endian output length followed by control bytes:
/// the high bit means "copy the next `(b & 0x7F) + 1` literal bytes", otherwise
/// emit `(b & 0x7F) + 3` zeros.
fn unsparse(data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() < 4 {
        return Err(Error::TruncatedSector);
    }
    let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut out = Vec::with_capacity(size);
    let mut i = 4;
    while i < data.len() && out.len() < size {
        let ctrl = data[i];
        i += 1;
        if ctrl & 0x80 != 0 {
            let n = (ctrl & 0x7F) as usize + 1;
            let end = (i + n).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else {
            out.resize(out.len() + (ctrl & 0x7F) as usize + 3, 0);
        }
    }
    out.truncate(size);
    Ok(out)
}
