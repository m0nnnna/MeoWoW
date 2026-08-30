// The client's icon.
//
// **This file is compiled twice**: once as a module of the viewer, which turns
// it into the window's icon at runtime, and once by `build.rs` through an
// `include!`, which turns it into the `.ico` Windows shows in Explorer and on
// the taskbar. A build script cannot depend on the crate it is building, so
// everything here leans only on `std` and on `png` -- which is a
// `[build-dependencies]` entry as well as a normal one for exactly this
// reason. Two icons that drifted apart would be worse than one.
//
// The art itself lives in `app-icon.png`, a 256x256 straight-RGBA master
// exported once from the source drawing and committed as the one deliberate
// exception to the tree's blanket `*.png` ignore -- it is this project's own
// mark, not anyone else's asset. Everything below is just resampling it down
// to whatever size a title bar, a taskbar or an `.ico` entry asks for.

/// The 256x256 RGBA master, PNG-encoded. Decoded on demand rather than kept
/// unpacked: it is read a handful of times per launch (one window icon, four
/// `.ico` entries) and never in a hot path.
const MASTER_PNG: &[u8] = include_bytes!("app-icon.png");

/// The master, decoded to straight (non-premultiplied) RGBA, with its side
/// length.
///
/// Panics rather than degrades: unlike the window icon as a whole -- which is
/// allowed to fall back to the platform default -- a master PNG that will not
/// decode is a broken build, and a silent all-transparent icon would be
/// indistinguishable from the feature being switched off.
fn master() -> (Vec<u8>, u32) {
    let mut reader = png::Decoder::new(MASTER_PNG)
        .read_info()
        .expect("app-icon.png is a valid PNG");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("app-icon.png decodes");
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "app-icon.png must be straight RGBA"
    );
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(info.width, info.height, "app-icon.png must be square");
    buf.truncate(info.buffer_size());
    (buf, info.width)
}

/// Draws the icon at `size` by `size`, as straight (non-premultiplied) RGBA.
///
/// Transparent outside the cat, because an icon with its own opaque background
/// is a coloured square in a taskbar whatever the taskbar's colour is -- the
/// master already has the transparency and this preserves it.
///
/// A box (area-average) downsample: every destination pixel is the mean of the
/// source pixels it covers, so a 16-pixel icon is a real reduction of the art
/// rather than a stack of jagged nearest-neighbour steps. The averaging is
/// done in premultiplied colour and then un-premultiplied, or a soft edge
/// where opaque ink meets transparent black would darken as it faded.
pub fn draw(size: u32) -> Vec<u8> {
    let (src, src_size) = master();
    if size == 0 {
        return Vec::new();
    }
    if size == src_size {
        return src;
    }
    let mut out = vec![0u8; (size * size * 4) as usize];
    let scale = src_size as f64 / size as f64;
    // Guaranteed below by the `size == src_size` short-circuit above and by
    // there being no icon size larger than the 256 master: every destination
    // pixel covers at least one whole source pixel on each axis.
    for dy in 0..size {
        let yi0 = (dy as f64 * scale).floor() as u32;
        let yi1 = (((dy + 1) as f64 * scale).ceil() as u32).min(src_size);
        for dx in 0..size {
            let xi0 = (dx as f64 * scale).floor() as u32;
            let xi1 = (((dx + 1) as f64 * scale).ceil() as u32).min(src_size);

            let (mut r, mut g, mut b, mut a) = (0.0f64, 0.0, 0.0, 0.0);
            let mut n = 0.0f64;
            for sy in yi0..yi1 {
                for sx in xi0..xi1 {
                    n += 1.0;
                    let i = ((sy * src_size + sx) * 4) as usize;
                    let alpha = src[i + 3] as f64;
                    r += src[i] as f64 * alpha;
                    g += src[i + 1] as f64 * alpha;
                    b += src[i + 2] as f64 * alpha;
                    a += alpha;
                }
            }

            let at = ((dy * size + dx) * 4) as usize;
            if a > 0.0 {
                out[at] = (r / a).round().clamp(0.0, 255.0) as u8;
                out[at + 1] = (g / a).round().clamp(0.0, 255.0) as u8;
                out[at + 2] = (b / a).round().clamp(0.0, 255.0) as u8;
            }
            if n > 0.0 {
                out[at + 3] = (a / n).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Packs one or more sizes into a Windows `.ico`.
///
/// Uncompressed 32-bit DIB entries rather than PNG payloads. Both are legal
/// and Windows has read PNG-in-ICO since Vista, but a DIB is understood by
/// every tool that has ever opened an icon, and the whole file at these sizes
/// is a few tens of kilobytes either way. There is no reason to spend
/// compatibility on nothing.
///
/// **The header stores a size of 0 to mean 256.** One byte per dimension, so
/// 256 does not fit -- a detail that silently truncates a large icon to
/// nothing if it is missed, which is why it is written out rather than assumed.
///
/// Called by `build.rs` through the `include!` above, and by this crate's own
/// tests. The crate proper never calls it, which is what the lint objects to.
#[allow(dead_code)]
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut directory = Vec::new();
    let mut images = Vec::new();
    // Reserved, type 1 (icon), count.
    directory.extend_from_slice(&[0, 0, 1, 0]);
    directory.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for &size in sizes {
        let image = dib(size);
        directory.push(if size >= 256 { 0 } else { size as u8 });
        directory.push(if size >= 256 { 0 } else { size as u8 });
        // No palette, no colour planes worth stating, 32 bits per pixel.
        directory.extend_from_slice(&[0, 0]);
        directory.extend_from_slice(&1u16.to_le_bytes());
        directory.extend_from_slice(&32u16.to_le_bytes());
        directory.extend_from_slice(&(image.len() as u32).to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
        images.push(image);
    }
    directory.extend(images.into_iter().flatten());
    directory
}

/// One icon image: a `BITMAPINFOHEADER`, bottom-up BGRA rows, and an AND mask.
///
/// Reached only through [`ico`], so it is unused for the same reason.
#[allow(dead_code)]
fn dib(size: u32) -> Vec<u8> {
    let rgba = draw(size);
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // header size
    out.extend_from_slice(&(size as i32).to_le_bytes());
    // **Twice the height**, because the header describes the colour rows and
    // the mask rows as one image. Writing the true height here is the classic
    // way to produce an icon that draws its bottom half over its top.
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes()); // image size, may be zero
    out.extend_from_slice(&[0u8; 16]); // resolution and palette counts

    for y in (0..size).rev() {
        for x in 0..size {
            let at = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[
                rgba[at + 2],
                rgba[at + 1],
                rgba[at],
                rgba[at + 3],
            ]);
        }
    }
    // The AND mask, all zero: every pixel is "not masked out", and the alpha
    // channel above is what actually shapes the icon. Rows are padded to four
    // bytes, which for a 1-bit mask matters at every size below 32.
    let stride = ((size + 31) / 32) * 4;
    out.extend(std::iter::repeat_n(0u8, (stride * size) as usize));
    out
}
