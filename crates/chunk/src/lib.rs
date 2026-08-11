//! Blizzard's chunked container: a flat sequence of `(magic, size, payload)`
//! records, shared by WMO and ADT.
//!
//! Being chunked is what makes both formats forgiving to read. Unknown chunks
//! are skipped rather than shifting everything after them, so a reader that
//! understands half the chunks still works.
//!
//! **Magics are stored reversed.** `MVER` appears in the file as `REVM`.
//! Identifiers are un-reversed on read so callers see the documented name.

/// A four-character chunk identifier, stored un-reversed.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Magic(pub [u8; 4]);

impl Magic {
    pub const fn new(s: &[u8; 4]) -> Self {
        Self(*s)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("????")
    }
}

impl std::fmt::Debug for Magic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::fmt::Display for Magic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Walks a chunked file.
///
/// Stops cleanly at the first truncated chunk rather than panicking, so a
/// half-written file yields the chunks that are intact.
pub struct Chunks<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Chunks<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Byte offset of the next chunk header, for formats that address chunks
    /// by position rather than by search.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Payload of the first chunk with this magic.
    pub fn find(data: &'a [u8], magic: &[u8; 4]) -> Option<&'a [u8]> {
        Chunks::new(data)
            .find(|(m, _)| m.0 == *magic)
            .map(|(_, payload)| payload)
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = (Magic, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let header = self.data.get(self.pos..self.pos + 8)?;
        // Stored little-endian, which reads as the identifier backwards.
        let magic = Magic([header[3], header[2], header[1], header[0]]);
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;

        let start = self.pos + 8;
        let payload = self.data.get(start..start + size)?;
        self.pos = start + size;
        Some((magic, payload))
    }
}

pub fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .unwrap_or(0)
}

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
        .unwrap_or(0)
}

pub fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32_at(b, o))
}

pub fn vec3_at(b: &[u8], o: usize) -> [f32; 3] {
    [f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)]
}

/// Reads a NUL-terminated string at a byte offset into a string block.
///
/// Both formats index names by byte offset rather than by ordinal, so this is
/// the usual way a filename is reached.
pub fn string_at(block: &[u8], offset: usize) -> &str {
    let Some(tail) = block.get(offset..) else {
        return "";
    };
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    std::str::from_utf8(&tail[..end]).unwrap_or("")
}

/// Splits a string block into every NUL-terminated entry, in order.
pub fn strings(block: &[u8]) -> Vec<&str> {
    block
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunked(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (magic, payload) in chunks {
            let m = *magic;
            out.extend_from_slice(&[m[3], m[2], m[1], m[0]]);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
        }
        out
    }

    #[test]
    fn reads_reversed_magics() {
        let data = chunked(&[(b"MVER", 18u32.to_le_bytes().to_vec())]);
        assert_eq!(&data[..4], b"REVM");
        let (magic, payload) = Chunks::new(&data).next().unwrap();
        assert_eq!(magic.as_str(), "MVER");
        assert_eq!(u32_at(payload, 0), 18);
    }

    #[test]
    fn skips_unknown_chunks() {
        let data = chunked(&[
            (b"MVER", 18u32.to_le_bytes().to_vec()),
            (b"ZZZZ", vec![9; 8]),
            (b"MHDR", vec![0; 64]),
        ]);
        assert!(Chunks::find(&data, b"MHDR").is_some());
        assert_eq!(Chunks::new(&data).count(), 3);
    }

    #[test]
    fn stops_at_a_truncated_chunk() {
        let mut data = chunked(&[
            (b"MVER", 18u32.to_le_bytes().to_vec()),
            (b"MHDR", vec![0; 64]),
        ]);
        data.truncate(data.len() - 20);
        assert_eq!(Chunks::new(&data).count(), 1);
    }

    #[test]
    fn splits_string_blocks() {
        let block = b"a.m2\0b.m2\0";
        assert_eq!(strings(block), vec!["a.m2", "b.m2"]);
        assert_eq!(string_at(block, 5), "b.m2");
    }
}
