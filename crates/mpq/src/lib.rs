//! Reader for Blizzard's MPQ archive format, versions 0 and 1.
//!
//! Written from the public format documentation at <https://wowdev.wiki/MPQ>.
//! Scope is deliberately read-only and limited to what a 3.3.5a client needs:
//! WoW ships format version 1 archives with zlib- and bzip2-compressed sectors.
//!
//! An archive stores no filenames. Each path is reduced to three hashes; the
//! first picks a slot in a power-of-two hash table and the other two verify it.
//! A conventional `(listfile)` member holds the real names, but it is only a
//! convention -- files absent from it are still readable if you know the path.

mod chain;
pub mod compress;
pub mod crypt;

pub use chain::Chain;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crypt::HashKind;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no MPQ header found (searched {searched} 512-byte boundaries)")]
    NoHeader { searched: u64 },
    #[error("unsupported MPQ format version {0} (this reader handles 0 and 1)")]
    UnsupportedVersion(u16),
    #[error("file not found in archive: {0}")]
    NotFound(String),
    #[error("sector data ended early")]
    TruncatedSector,
    #[error("unsupported compression mask {0:#04x}")]
    UnsupportedCompression(u8),
    #[error("decompression failed: {0}")]
    Decompress(String),
    #[error("{0} is an incremental patch file, which is not supported yet")]
    IncrementalPatch(String),
    #[error("{path} decompressed to {got} bytes, expected {want}")]
    SizeMismatch {
        path: String,
        got: usize,
        want: usize,
    },
}

mod flags {
    pub const IMPLODE: u32 = 0x0000_0100;
    pub const COMPRESS: u32 = 0x0000_0200;
    pub const ENCRYPTED: u32 = 0x0001_0000;
    pub const FIX_KEY: u32 = 0x0002_0000;
    pub const PATCH_FILE: u32 = 0x0010_0000;
    pub const SINGLE_UNIT: u32 = 0x0100_0000;
    pub const DELETE_MARKER: u32 = 0x0200_0000;
    pub const SECTOR_CRC: u32 = 0x0400_0000;
    pub const EXISTS: u32 = 0x8000_0000;
}

const HASH_ENTRY_EMPTY: u32 = 0xFFFF_FFFF;
const HASH_ENTRY_DELETED: u32 = 0xFFFF_FFFE;

#[derive(Clone, Copy, Debug)]
struct HashEntry {
    name_a: u32,
    name_b: u32,
    locale: u16,
    block_index: u32,
}

#[derive(Clone, Copy, Debug)]
struct BlockEntry {
    offset: u64,
    packed_size: u32,
    size: u32,
    flags: u32,
}

/// Metadata for one archive member.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// Size after decompression.
    pub size: u32,
    /// Size as stored.
    pub packed_size: u32,
    pub compressed: bool,
    pub encrypted: bool,
    pub locale: u16,
}

/// A single opened `.MPQ` file.
pub struct Archive {
    path: PathBuf,
    file: File,
    /// Archives may be appended to another file, so all stored offsets are
    /// relative to where the header was actually found.
    base: u64,
    sector_size: usize,
    hash_table: Vec<HashEntry>,
    block_table: Vec<BlockEntry>,
}

impl Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let len = file
            .metadata()
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?
            .len();

        let (base, header) = find_header(&mut file, len, &path)?;

        if header.format_version > 1 {
            return Err(Error::UnsupportedVersion(header.format_version));
        }

        let hash_pos = base + combine(header.hash_table_pos, header.hash_table_pos_hi);
        let block_pos = base + combine(header.block_table_pos, header.block_table_pos_hi);

        let hash_table = read_hash_table(&mut file, &path, hash_pos, header.hash_table_count)?;
        let block_table =
            read_block_table(&mut file, &path, block_pos, header.block_table_count, base)?;

        tracing::debug!(
            path = %path.display(),
            version = header.format_version,
            sector = 512usize << header.sector_size_shift,
            hash_slots = hash_table.len(),
            blocks = block_table.len(),
            "opened archive"
        );

        Ok(Self {
            path,
            file,
            base,
            sector_size: 512usize << header.sector_size_shift,
            hash_table,
            block_table,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Locates a path's hash slot by open addressing.
    ///
    /// An empty slot terminates the probe (nothing was ever inserted past it),
    /// but a deleted slot does not -- a later insertion may have probed through
    /// it.
    fn find(&self, name: &str) -> Option<&HashEntry> {
        if self.hash_table.is_empty() {
            return None;
        }
        let mask = self.hash_table.len() - 1;
        let start = crypt::hash(name, HashKind::TableOffset) as usize & mask;
        let a = crypt::hash(name, HashKind::NameA);
        let b = crypt::hash(name, HashKind::NameB);

        for i in 0..self.hash_table.len() {
            let entry = &self.hash_table[(start + i) & mask];
            if entry.block_index == HASH_ENTRY_EMPTY {
                return None;
            }
            if entry.block_index != HASH_ENTRY_DELETED && entry.name_a == a && entry.name_b == b {
                return Some(entry);
            }
        }
        None
    }

    fn block_of(&self, name: &str) -> Option<(BlockEntry, u16)> {
        let hash = self.find(name)?;
        let block = *self.block_table.get(hash.block_index as usize)?;
        (block.flags & flags::EXISTS != 0 && block.flags & flags::DELETE_MARKER == 0)
            .then_some((block, hash.locale))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.block_of(name).is_some()
    }

    pub fn stat(&self, name: &str) -> Option<Entry> {
        let (block, locale) = self.block_of(name)?;
        Some(Entry {
            size: block.size,
            packed_size: block.packed_size,
            compressed: block.flags & (flags::COMPRESS | flags::IMPLODE) != 0,
            encrypted: block.flags & flags::ENCRYPTED != 0,
            locale,
        })
    }

    /// Reads and fully decodes a member.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        let (block, _) = self
            .block_of(name)
            .ok_or_else(|| Error::NotFound(name.to_string()))?;

        if block.flags & flags::PATCH_FILE != 0 {
            return Err(Error::IncrementalPatch(name.to_string()));
        }

        let key = if block.flags & flags::ENCRYPTED != 0 {
            let k = crypt::file_key(name);
            Some(if block.flags & flags::FIX_KEY != 0 {
                // The stored offset, not the absolute one, feeds the mix.
                crypt::fix_key(k, (block.offset - self.base) as u32, block.size)
            } else {
                k
            })
        } else {
            None
        };

        let out = if block.flags & flags::SINGLE_UNIT != 0 {
            self.read_single_unit(&block, key)?
        } else if block.flags & (flags::COMPRESS | flags::IMPLODE) != 0 {
            self.read_sectored(&block, key)?
        } else {
            let mut raw = self.read_at(block.offset, block.size as usize)?;
            if let Some(key) = key {
                decrypt_bytes(&mut raw, key);
            }
            raw
        };

        if out.len() != block.size as usize {
            return Err(Error::SizeMismatch {
                path: name.to_string(),
                got: out.len(),
                want: block.size as usize,
            });
        }
        Ok(out)
    }

    fn read_single_unit(&mut self, block: &BlockEntry, key: Option<u32>) -> Result<Vec<u8>, Error> {
        let mut raw = self.read_at(block.offset, block.packed_size as usize)?;
        if let Some(key) = key {
            decrypt_bytes(&mut raw, key);
        }
        if block.flags & (flags::COMPRESS | flags::IMPLODE) != 0
            && block.packed_size < block.size
        {
            compress::decompress(&raw, block.size as usize)
        } else {
            Ok(raw)
        }
    }

    fn read_sectored(&mut self, block: &BlockEntry, key: Option<u32>) -> Result<Vec<u8>, Error> {
        let count = (block.size as usize).div_ceil(self.sector_size);
        // One offset per sector plus a terminator, and one more when per-sector
        // checksums are appended as a trailing pseudo-sector.
        let n_offsets = count + 1 + usize::from(block.flags & flags::SECTOR_CRC != 0);

        let mut table = self.read_at(block.offset, n_offsets * 4)?;
        if let Some(key) = key {
            // The offset table is keyed one step below the data.
            decrypt_bytes(&mut table, key.wrapping_sub(1));
        }
        let offsets: Vec<u32> = table
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut out = Vec::with_capacity(block.size as usize);
        for i in 0..count {
            let (start, end) = (offsets[i] as usize, offsets[i + 1] as usize);
            if end < start || end > block.packed_size as usize {
                return Err(Error::TruncatedSector);
            }
            let expected = self.sector_size.min(block.size as usize - out.len());

            let mut raw = self.read_at(block.offset + start as u64, end - start)?;
            if let Some(key) = key {
                decrypt_bytes(&mut raw, key.wrapping_add(i as u32));
            }

            // A sector that did not shrink was stored verbatim; there is no
            // compression mask byte in front of it.
            if raw.len() >= expected {
                out.extend_from_slice(&raw[..expected]);
            } else {
                out.extend_from_slice(&compress::decompress(&raw, expected)?);
            }
        }
        Ok(out)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; len];
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut buf))
            .map_err(|source| Error::Io {
                path: self.path.clone(),
                source,
            })?;
        Ok(buf)
    }

    /// Returns the paths named by the archive's `(listfile)`, if it has one.
    ///
    /// This is the only way to enumerate names: the hash table proves a path
    /// exists but cannot reproduce it.
    pub fn list(&mut self) -> Result<Vec<String>, Error> {
        let raw = match self.read("(listfile)") {
            Ok(raw) => raw,
            Err(Error::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let text = String::from_utf8_lossy(&raw);
        let mut names: Vec<String> = text
            .split(['\r', '\n'])
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        names.sort_unstable();
        names.dedup();
        Ok(names)
    }
}

fn decrypt_bytes(buf: &mut [u8], key: u32) {
    let words = buf.len() / 4;
    let mut tmp: Vec<u32> = buf[..words * 4]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    crypt::decrypt(&mut tmp, key);
    for (dst, word) in buf.chunks_exact_mut(4).zip(tmp) {
        dst.copy_from_slice(&word.to_le_bytes());
    }
    // A trailing partial word is left as-is: the cipher never covered it.
}

#[derive(Debug, Default)]
struct Header {
    format_version: u16,
    sector_size_shift: u16,
    hash_table_pos: u32,
    block_table_pos: u32,
    hash_table_count: u32,
    block_table_count: u32,
    hash_table_pos_hi: u16,
    block_table_pos_hi: u16,
}

fn combine(lo: u32, hi: u16) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

const MAGIC_HEADER: [u8; 4] = *b"MPQ\x1a";
const MAGIC_USER_DATA: [u8; 4] = *b"MPQ\x1b";

/// Scans 512-byte boundaries for the header, since an archive can be appended
/// to an installer or another file.
fn find_header(file: &mut File, len: u64, path: &Path) -> Result<(u64, Header), Error> {
    let mut offset = 0u64;
    let mut searched = 0u64;
    let mut buf = [0u8; 44];

    while offset + 32 <= len {
        searched += 1;
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let n = read_upto(file, &mut buf, path)?;
        if n >= 32 && buf[..4] == MAGIC_HEADER {
            return Ok((offset, parse_header(&buf[..n])));
        }
        if n >= 16 && buf[..4] == MAGIC_USER_DATA {
            // A user-data block points at where the real header begins.
            let rel = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as u64;
            offset += rel;
            continue;
        }
        offset += 512;
    }
    Err(Error::NoHeader { searched })
}

fn read_upto(file: &mut File, buf: &mut [u8; 44], path: &Path) -> Result<usize, Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        }
    }
    Ok(filled)
}

fn parse_header(b: &[u8]) -> Header {
    let u16at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

    let mut h = Header {
        format_version: u16at(0x0C),
        sector_size_shift: u16at(0x0E),
        hash_table_pos: u32at(0x10),
        block_table_pos: u32at(0x14),
        hash_table_count: u32at(0x18),
        block_table_count: u32at(0x1C),
        ..Default::default()
    };
    // Version 1 widens the table offsets past 4 GiB.
    if h.format_version >= 1 && b.len() >= 44 {
        h.hash_table_pos_hi = u16at(0x28);
        h.block_table_pos_hi = u16at(0x2A);
    }
    h
}

fn read_hash_table(
    file: &mut File,
    path: &Path,
    pos: u64,
    count: u32,
) -> Result<Vec<HashEntry>, Error> {
    let words = read_encrypted_table(file, path, pos, count, "(hash table)")?;
    Ok(words
        .chunks_exact(4)
        .map(|c| HashEntry {
            name_a: c[0],
            name_b: c[1],
            locale: (c[2] & 0xFFFF) as u16,
            block_index: c[3],
        })
        .collect())
}

fn read_block_table(
    file: &mut File,
    path: &Path,
    pos: u64,
    count: u32,
    base: u64,
) -> Result<Vec<BlockEntry>, Error> {
    let words = read_encrypted_table(file, path, pos, count, "(block table)")?;
    Ok(words
        .chunks_exact(4)
        .map(|c| BlockEntry {
            // Stored relative to the header; kept absolute in memory so every
            // read goes straight to the file. `FIX_KEY` needs the relative form
            // and subtracts the base back out.
            offset: base + c[0] as u64,
            packed_size: c[1],
            size: c[2],
            flags: c[3],
        })
        .collect())
}

fn read_encrypted_table(
    file: &mut File,
    path: &Path,
    pos: u64,
    count: u32,
    key_name: &str,
) -> Result<Vec<u32>, Error> {
    let bytes = count as usize * 16;
    let mut buf = vec![0u8; bytes];
    file.seek(SeekFrom::Start(pos))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let mut words: Vec<u32> = buf
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    crypt::decrypt(&mut words, crypt::hash(key_name, HashKind::FileKey));
    Ok(words)
}
