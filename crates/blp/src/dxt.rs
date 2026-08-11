//! S3TC / BC1-3 block decoding.
//!
//! Implemented in-tree rather than pulled in, because the renderer's fast path
//! never calls it: `wgpu` uploads these blocks to the GPU compressed, and the
//! hardware samples them directly. This exists for tooling, tests, and the few
//! places that genuinely need pixels on the CPU.
//!
//! Each format packs a 4x4 block. Colours are two RGB565 endpoints plus 2-bit
//! per-pixel interpolation weights; the alpha schemes differ per format.

/// Which block-compressed layout a texture uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DxtFormat {
    /// BC1. 8 bytes per block, optional 1-bit alpha.
    Dxt1,
    /// BC2. 16 bytes per block, 4-bit uninterpolated alpha.
    Dxt3,
    /// BC3. 16 bytes per block, interpolated alpha.
    Dxt5,
}

impl DxtFormat {
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::Dxt1 => 8,
            Self::Dxt3 | Self::Dxt5 => 16,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Dxt1 => "DXT1",
            Self::Dxt3 => "DXT3",
            Self::Dxt5 => "DXT5",
        }
    }

    /// Bytes one mip level occupies. Dimensions round up to whole blocks, so a
    /// 1x1 mip still costs a full block.
    pub const fn level_bytes(self, width: u32, height: u32) -> usize {
        let bw = width.div_ceil(4) as usize;
        let bh = height.div_ceil(4) as usize;
        bw * bh * self.block_bytes()
    }
}

#[inline]
fn rgb565(value: u16) -> [u8; 3] {
    let r = ((value >> 11) & 0x1F) as u8;
    let g = ((value >> 5) & 0x3F) as u8;
    let b = (value & 0x1F) as u8;
    // Replicate the high bits into the low ones so 0x1F maps to 0xFF rather
    // than 0xF8; a plain shift darkens every texture slightly.
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

/// Expands one block's colour endpoints into its four-entry palette.
///
/// When `punchthrough` is allowed and the endpoints are ordered low-to-high,
/// the block trades its fourth colour for transparency -- this is how DXT1
/// encodes 1-bit alpha.
fn color_palette(block: &[u8], punchthrough: bool) -> ([[u8; 3]; 4], bool) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));

    let mut palette = [[0u8; 3]; 4];
    palette[0] = a;
    palette[1] = b;

    let transparent = punchthrough && c0 <= c1;
    if transparent {
        for i in 0..3 {
            palette[2][i] = ((a[i] as u16 + b[i] as u16) / 2) as u8;
        }
        palette[3] = [0, 0, 0];
    } else {
        for i in 0..3 {
            palette[2][i] = ((2 * a[i] as u16 + b[i] as u16) / 3) as u8;
            palette[3][i] = ((a[i] as u16 + 2 * b[i] as u16) / 3) as u8;
        }
    }
    (palette, transparent)
}

/// Decodes a DXT surface into tightly packed RGBA8.
///
/// `data` must hold whole blocks; a short buffer decodes what it can and
/// leaves the remainder transparent, which keeps a truncated mip from taking
/// down a whole texture load.
pub fn decode(format: DxtFormat, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let (bw, bh) = (width.div_ceil(4) as usize, height.div_ceil(4) as usize);
    let stride = format.block_bytes();

    for by in 0..bh {
        for bx in 0..bw {
            let offset = (by * bw + bx) * stride;
            let Some(block) = data.get(offset..offset + stride) else {
                continue;
            };

            // The colour half sits after the alpha half in DXT3 and DXT5.
            let color = if format == DxtFormat::Dxt1 {
                block
            } else {
                &block[8..]
            };
            let (palette, punchthrough) =
                color_palette(color, format == DxtFormat::Dxt1);
            let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);

            for py in 0..4 {
                for px in 0..4 {
                    let (x, y) = (bx * 4 + px, by * 4 + py);
                    if x >= w || y >= h {
                        continue;
                    }
                    let i = py * 4 + px;
                    let code = ((bits >> (2 * i)) & 0x3) as usize;
                    let rgb = palette[code];

                    let alpha = match format {
                        DxtFormat::Dxt1 => {
                            if punchthrough && code == 3 {
                                0
                            } else {
                                255
                            }
                        }
                        DxtFormat::Dxt3 => {
                            // 4 bits per pixel, two pixels per byte.
                            let nibble = block[i / 2];
                            let v = if i % 2 == 0 {
                                nibble & 0x0F
                            } else {
                                nibble >> 4
                            };
                            // Replicate rather than shift, so 0xF reaches 0xFF.
                            v * 17
                        }
                        DxtFormat::Dxt5 => alpha_dxt5(block, i),
                    };

                    let o = (y * w + x) * 4;
                    out[o] = rgb[0];
                    out[o + 1] = rgb[1];
                    out[o + 2] = rgb[2];
                    out[o + 3] = alpha;
                }
            }
        }
    }
    out
}

/// DXT5 alpha: two endpoints and 3-bit indices into an interpolated ramp.
///
/// As with colour, endpoint ordering selects between a 6-value ramp with
/// explicit 0 and 255 entries, and a full 8-value ramp.
fn alpha_dxt5(block: &[u8], pixel: usize) -> u8 {
    let (a0, a1) = (block[0], block[1]);
    let mut ramp = [0u16; 8];
    ramp[0] = a0 as u16;
    ramp[1] = a1 as u16;
    if a0 > a1 {
        for i in 1..7 {
            ramp[i + 1] = ((7 - i as u16) * a0 as u16 + i as u16 * a1 as u16) / 7;
        }
    } else {
        for i in 1..5 {
            ramp[i + 1] = ((5 - i as u16) * a0 as u16 + i as u16 * a1 as u16) / 5;
        }
        ramp[6] = 0;
        ramp[7] = 255;
    }

    // 16 three-bit indices packed into six bytes, little-endian.
    let bits = u64::from_le_bytes([
        block[2], block[3], block[4], block[5], block[6], block[7], 0, 0,
    ]);
    let code = ((bits >> (3 * pixel)) & 0x7) as usize;
    ramp[code] as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block whose endpoints are both pure red must decode to pure red
    /// everywhere, whichever interpolation weight each pixel selects.
    #[test]
    fn decodes_a_uniform_dxt1_block() {
        let red = 0xF800u16.to_le_bytes();
        let mut block = Vec::new();
        block.extend_from_slice(&red);
        block.extend_from_slice(&red);
        block.extend_from_slice(&[0x00; 4]);

        let out = decode(DxtFormat::Dxt1, &block, 4, 4);
        for px in out.chunks_exact(4) {
            assert_eq!(px, [255, 0, 0, 255]);
        }
    }

    /// Endpoints in ascending order put DXT1 in punchthrough mode, where index
    /// 3 is transparent black.
    #[test]
    fn dxt1_punchthrough_marks_index_three_transparent() {
        let mut block = Vec::new();
        block.extend_from_slice(&0x0000u16.to_le_bytes()); // c0 <= c1
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        // Every pixel selects index 3.
        block.extend_from_slice(&[0xFF; 4]);

        let out = decode(DxtFormat::Dxt1, &block, 4, 4);
        for px in out.chunks_exact(4) {
            assert_eq!(px[3], 0, "index 3 must be transparent in this mode");
        }
    }

    /// 565 endpoints must saturate: the maximum channel value has to reach
    /// 255, not 248, or every texture comes out subtly dark.
    #[test]
    fn rgb565_saturates() {
        assert_eq!(rgb565(0xFFFF), [255, 255, 255]);
        assert_eq!(rgb565(0x0000), [0, 0, 0]);
    }

    /// DXT3's 4-bit alpha must expand so full opacity is 255.
    #[test]
    fn dxt3_alpha_saturates() {
        let mut block = vec![0xFFu8; 8]; // all alpha nibbles = 0xF
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&[0x00; 4]);

        let out = decode(DxtFormat::Dxt3, &block, 4, 4);
        for px in out.chunks_exact(4) {
            assert_eq!(px[3], 255);
        }
    }

    /// The two DXT5 ramp modes differ in whether they reserve slots for fully
    /// transparent and fully opaque.
    #[test]
    fn dxt5_alpha_ramp_modes() {
        // a0 > a1 -> 8 interpolated values, no reserved entries.
        let mut block = vec![255u8, 0u8];
        block.extend_from_slice(&[0x00; 6]); // every index 0 -> a0
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&[0x00; 4]);
        let out = decode(DxtFormat::Dxt5, &block, 4, 4);
        assert_eq!(out[3], 255);

        // a0 <= a1 -> index 6 is a reserved 0.
        let mut block = vec![0u8, 255u8];
        // Index 6 for pixel 0: 0b110.
        block.extend_from_slice(&[0x06, 0x00, 0x00, 0x00, 0x00, 0x00]);
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&0xFFFFu16.to_le_bytes());
        block.extend_from_slice(&[0x00; 4]);
        let out = decode(DxtFormat::Dxt5, &block, 4, 4);
        assert_eq!(out[3], 0, "index 6 is reserved transparent in this mode");
    }

    /// Non-multiple-of-four dimensions still occupy whole blocks.
    #[test]
    fn partial_blocks_are_sized_up() {
        assert_eq!(DxtFormat::Dxt1.level_bytes(1, 1), 8);
        assert_eq!(DxtFormat::Dxt5.level_bytes(5, 5), 4 * 16);
    }
}
