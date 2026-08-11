//! Reader for Blizzard's BLP2 texture format.
//!
//! Written from the public format documentation at <https://wowdev.wiki/BLP>.
//!
//! A BLP is a header, a 256-entry palette, and up to 16 mip levels. Pixels are
//! stored one of three ways: block-compressed (DXT1/3/5), 8-bit palette
//! indices with a separate alpha plane, or raw BGRA.
//!
//! The API deliberately separates [`Blp::level`], which hands back bytes
//! exactly as stored, from [`Blp::decode_rgba`], which expands them. The
//! renderer wants the former -- DXT blocks upload to the GPU compressed and are
//! sampled by the hardware, so decoding them on the CPU would waste both time
//! and four times the memory.

pub mod dxt;

pub use dxt::DxtFormat;

/// Bytes before the palette.
const HEADER_SIZE: usize = 0x94;
/// Palette is always present and always full size, whatever the encoding.
const PALETTE_BYTES: usize = 256 * 4;
/// Header plus palette; where mip data can start.
pub const DATA_START: usize = HEADER_SIZE + PALETTE_BYTES;
/// The header has room for exactly this many mips.
const MAX_MIPS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a BLP2 file (magic {0:?})")]
    BadMagic([u8; 4]),
    #[error("file is {got} bytes, too short for a {DATA_START}-byte header")]
    TooShort { got: usize },
    #[error("JPEG-compressed BLPs are not supported (content type {0})")]
    JpegContent(u32),
    #[error("unknown pixel encoding {0}")]
    UnknownEncoding(u8),
    #[error(
        "unsupported DXT variant: alpha_depth={alpha_depth}, alpha_type={alpha_type}"
    )]
    UnsupportedDxt { alpha_depth: u8, alpha_type: u8 },
    #[error("unsupported alpha depth {0} for a palettized texture")]
    UnsupportedAlphaDepth(u8),
    #[error("{0}x{1} is not a usable texture size")]
    BadDimensions(u32, u32),
    #[error("mip {level} runs from {offset} to {end}, past the {len}-byte file")]
    MipOutOfBounds {
        level: usize,
        offset: usize,
        end: usize,
        len: usize,
    },
}

/// How pixels are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 8-bit indices into the palette, with alpha in a separate plane.
    Palettized,
    /// Block-compressed.
    Dxt(DxtFormat),
    /// Raw 32-bit BGRA.
    Bgra,
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Self::Palettized => "palettized",
            Self::Dxt(f) => f.name(),
            Self::Bgra => "BGRA8888",
        }
    }
}

/// One mip level's bytes, exactly as stored.
#[derive(Clone, Copy, Debug)]
pub enum Level<'a> {
    Dxt {
        format: DxtFormat,
        blocks: &'a [u8],
    },
    Palettized {
        indices: &'a [u8],
        /// Packed at `alpha_depth` bits per pixel; empty when there is none.
        alpha: &'a [u8],
        alpha_depth: u8,
    },
    Bgra(&'a [u8]),
}

/// A parsed texture.
pub struct Blp {
    width: u32,
    height: u32,
    encoding: Encoding,
    alpha_depth: u8,
    /// Palette entries as BGRA, indexed directly by a palettized pixel.
    palette: Vec<[u8; 4]>,
    mips: Vec<(usize, usize)>,
    data: Vec<u8>,
}

impl Blp {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < DATA_START {
            return Err(Error::TooShort { got: bytes.len() });
        }
        if &bytes[..4] != b"BLP2" {
            return Err(Error::BadMagic([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
        let word = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());

        // Content type 0 is JPEG, which Blizzard stopped using before this
        // client; anything but 1 means we are misreading the file.
        let content = word(0x04);
        if content != 1 {
            return Err(Error::JpegContent(content));
        }

        let (encoding_id, alpha_depth, alpha_type) = (bytes[0x08], bytes[0x09], bytes[0x0A]);
        let (width, height) = (word(0x0C), word(0x10));
        if width == 0 || height == 0 || width > 1 << 15 || height > 1 << 15 {
            return Err(Error::BadDimensions(width, height));
        }

        let encoding = match encoding_id {
            1 => {
                if !matches!(alpha_depth, 0 | 1 | 4 | 8) {
                    return Err(Error::UnsupportedAlphaDepth(alpha_depth));
                }
                Encoding::Palettized
            }
            2 => Encoding::Dxt(dxt_format(alpha_depth, alpha_type)?),
            3 => Encoding::Bgra,
            other => return Err(Error::UnknownEncoding(other)),
        };

        let palette = bytes[HEADER_SIZE..HEADER_SIZE + PALETTE_BYTES]
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();

        // A level is present when it has both an offset and a size; the header
        // reserves 16 slots and zero-fills the unused tail.
        let mut mips = Vec::new();
        for i in 0..MAX_MIPS {
            let offset = word(0x14 + i * 4) as usize;
            let size = word(0x54 + i * 4) as usize;
            if offset == 0 || size == 0 {
                break;
            }
            let end = offset.saturating_add(size);
            if end > bytes.len() {
                return Err(Error::MipOutOfBounds {
                    level: i,
                    offset,
                    end,
                    len: bytes.len(),
                });
            }
            mips.push((offset, size));
        }

        Ok(Self {
            width,
            height,
            encoding,
            alpha_depth,
            palette,
            mips,
            data: bytes.to_vec(),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn alpha_depth(&self) -> u8 {
        self.alpha_depth
    }

    pub fn mip_count(&self) -> usize {
        self.mips.len()
    }

    /// Dimensions of a mip level. Each level halves, clamped so the chain ends
    /// at 1x1 rather than 0.
    pub fn level_size(&self, level: usize) -> (u32, u32) {
        (
            (self.width >> level).max(1),
            (self.height >> level).max(1),
        )
    }

    /// Bytes a level *should* occupy, given its dimensions and encoding.
    pub fn expected_level_bytes(&self, level: usize) -> usize {
        let (w, h) = self.level_size(level);
        let pixels = (w as usize) * (h as usize);
        match self.encoding {
            Encoding::Dxt(format) => format.level_bytes(w, h),
            Encoding::Bgra => pixels * 4,
            Encoding::Palettized => {
                pixels + (pixels * self.alpha_depth as usize).div_ceil(8)
            }
        }
    }

    /// Leading mip levels whose stored size matches their dimensions.
    ///
    /// The mip chain is not always as long as it claims. Blizzard's tooling
    /// stops generating real levels once a dimension falls below one 4-pixel
    /// block row, then pads every remaining entry with a single dummy block --
    /// `UI-Achievement-MetalBorder-Top.blp` is 512x16 and declares 10 levels,
    /// but only the first three hold real data; levels 4 to 6 are byte-for-byte
    /// identical filler.
    ///
    /// Upload only this many levels. Handing the GPU a 16-byte buffer described
    /// as a 64x2 DXT5 surface reads past the end of it.
    pub fn usable_mip_count(&self) -> usize {
        (0..self.mips.len())
            .take_while(|&level| self.mips[level].1 == self.expected_level_bytes(level))
            .count()
    }

    /// A level's bytes as stored, for uploading straight to the GPU.
    pub fn level(&self, level: usize) -> Option<Level<'_>> {
        let &(offset, size) = self.mips.get(level)?;
        let bytes = &self.data[offset..offset + size];
        let (w, h) = self.level_size(level);

        Some(match self.encoding {
            Encoding::Dxt(format) => Level::Dxt {
                format,
                blocks: bytes,
            },
            Encoding::Bgra => Level::Bgra(bytes),
            Encoding::Palettized => {
                // Indices come first, then the alpha plane fills the rest.
                let pixels = (w as usize) * (h as usize);
                let split = pixels.min(bytes.len());
                let (indices, alpha) = bytes.split_at(split);
                Level::Palettized {
                    indices,
                    alpha,
                    alpha_depth: self.alpha_depth,
                }
            }
        })
    }

    /// Expands a level to tightly packed RGBA8.
    pub fn decode_rgba(&self, level: usize) -> Option<Vec<u8>> {
        let (w, h) = self.level_size(level);
        Some(match self.level(level)? {
            Level::Dxt { format, blocks } => dxt::decode(format, blocks, w, h),
            Level::Bgra(src) => {
                let mut out = vec![0u8; (w * h) as usize * 4];
                for (dst, px) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    // Stored BGRA; swizzle to RGBA.
                    dst.copy_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                out
            }
            Level::Palettized {
                indices,
                alpha,
                alpha_depth,
            } => self.decode_palettized(indices, alpha, alpha_depth, w, h),
        })
    }

    fn decode_palettized(
        &self,
        indices: &[u8],
        alpha: &[u8],
        alpha_depth: u8,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let pixels = (width * height) as usize;
        let mut out = vec![0u8; pixels * 4];

        for i in 0..pixels {
            let entry = indices
                .get(i)
                .and_then(|&idx| self.palette.get(idx as usize))
                .copied()
                .unwrap_or([0, 0, 0, 0]);

            let a = match alpha_depth {
                0 => 255,
                1 => {
                    // One bit per pixel, least significant bit first.
                    let bit = alpha.get(i / 8).map_or(0, |b| (b >> (i % 8)) & 1);
                    if bit == 1 {
                        255
                    } else {
                        0
                    }
                }
                4 => {
                    // Two pixels per byte, low nibble first. Replicated so 0xF
                    // reaches full opacity.
                    let byte = alpha.get(i / 2).copied().unwrap_or(0);
                    let v = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                    v * 17
                }
                _ => alpha.get(i).copied().unwrap_or(255),
            };

            let o = i * 4;
            // Palette entries are BGRA.
            out[o] = entry[2];
            out[o + 1] = entry[1];
            out[o + 2] = entry[0];
            out[o + 3] = a;
        }
        out
    }
}

/// Picks the block format from the two alpha fields.
///
/// `alpha_type` is the real discriminator, but a texture with no alpha at all
/// is DXT1 regardless of what it claims.
fn dxt_format(alpha_depth: u8, alpha_type: u8) -> Result<DxtFormat, Error> {
    Ok(match (alpha_depth, alpha_type) {
        (0, _) => DxtFormat::Dxt1,
        (_, 0) => DxtFormat::Dxt1,
        (_, 1) => DxtFormat::Dxt3,
        (_, 7) => DxtFormat::Dxt5,
        _ => {
            return Err(Error::UnsupportedDxt {
                alpha_depth,
                alpha_type,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal single-mip BLP so parsing can be tested without game
    /// data.
    fn build(encoding: u8, alpha_depth: u8, alpha_type: u8, w: u32, h: u32, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; DATA_START];
        out[..4].copy_from_slice(b"BLP2");
        out[4..8].copy_from_slice(&1u32.to_le_bytes());
        out[8] = encoding;
        out[9] = alpha_depth;
        out[10] = alpha_type;
        out[11] = 0;
        out[0x0C..0x10].copy_from_slice(&w.to_le_bytes());
        out[0x10..0x14].copy_from_slice(&h.to_le_bytes());
        out[0x14..0x18].copy_from_slice(&(DATA_START as u32).to_le_bytes());
        out[0x54..0x58].copy_from_slice(&(body.len() as u32).to_le_bytes());
        // Palette entry 1 is opaque red, stored BGRA.
        out[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&[0, 0, 255, 255]);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn rejects_non_blp2() {
        let mut bytes = build(2, 0, 0, 4, 4, &[0; 8]);
        bytes[..4].copy_from_slice(b"BLP1");
        assert!(matches!(Blp::parse(&bytes), Err(Error::BadMagic(_))));
    }

    #[test]
    fn rejects_jpeg_content() {
        let mut bytes = build(2, 0, 0, 4, 4, &[0; 8]);
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(Blp::parse(&bytes), Err(Error::JpegContent(0))));
    }

    /// A mip pointing past the end must fail at parse time rather than when
    /// the level is later read.
    #[test]
    fn rejects_out_of_bounds_mip() {
        let mut bytes = build(2, 0, 0, 4, 4, &[0; 8]);
        bytes[0x54..0x58].copy_from_slice(&9999u32.to_le_bytes());
        assert!(matches!(Blp::parse(&bytes), Err(Error::MipOutOfBounds { .. })));
    }

    #[test]
    fn palettized_resolves_through_the_palette() {
        // 2x2 of palette index 1, no alpha plane.
        let bytes = build(1, 0, 0, 2, 2, &[1, 1, 1, 1]);
        let blp = Blp::parse(&bytes).unwrap();
        assert_eq!(blp.encoding(), Encoding::Palettized);
        let rgba = blp.decode_rgba(0).unwrap();
        for px in rgba.chunks_exact(4) {
            assert_eq!(px, [255, 0, 0, 255], "palette is BGRA, output is RGBA");
        }
    }

    /// One-bit alpha is packed LSB-first, so pixel 0 is the low bit.
    #[test]
    fn palettized_one_bit_alpha() {
        let bytes = build(1, 1, 0, 4, 1, &[1, 1, 1, 1, 0b0000_0101]);
        let blp = Blp::parse(&bytes).unwrap();
        let rgba = blp.decode_rgba(0).unwrap();
        let alphas: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(alphas, [255, 0, 255, 0]);
    }

    #[test]
    fn bgra_is_swizzled_to_rgba() {
        let bytes = build(3, 8, 0, 1, 1, &[10, 20, 30, 40]);
        let blp = Blp::parse(&bytes).unwrap();
        assert_eq!(blp.decode_rgba(0).unwrap(), vec![30, 20, 10, 40]);
    }

    #[test]
    fn mip_sizes_halve_and_clamp() {
        let bytes = build(2, 0, 0, 8, 4, &[0; 16]);
        let blp = Blp::parse(&bytes).unwrap();
        assert_eq!(blp.level_size(0), (8, 4));
        assert_eq!(blp.level_size(1), (4, 2));
        assert_eq!(blp.level_size(3), (1, 1));
        assert_eq!(blp.level_size(9), (1, 1), "never collapses to zero");
    }

    #[test]
    fn picks_the_dxt_variant_from_the_alpha_fields() {
        assert_eq!(dxt_format(0, 0).unwrap(), DxtFormat::Dxt1);
        assert_eq!(dxt_format(1, 0).unwrap(), DxtFormat::Dxt1);
        assert_eq!(dxt_format(8, 1).unwrap(), DxtFormat::Dxt3);
        assert_eq!(dxt_format(8, 7).unwrap(), DxtFormat::Dxt5);
        assert!(dxt_format(8, 3).is_err());
    }
}
