// The client's icon, drawn rather than shipped.
//
// **This file is compiled twice**: once as a module of the viewer, which
// turns it into the window's icon at runtime, and once by `build.rs` through
// an `include!`, which turns it into the `.ico` Windows shows in Explorer and
// on the taskbar. That is the whole reason it uses nothing but `std` and
// knows about neither `winit` nor the ICO format -- a build script cannot
// depend on the crate it is building, and two hand-drawn cats that drifted
// apart would be worse than one.
//
// Procedural for the same reason every frame in `crates/ui` is painted from
// explicit geometry: there is no art file to lose, it renders at whatever
// size is asked for, and the colours are the ones the interface already uses.

/// The cat's fur, which is the neko theme's accent (`#ff9ec4`).
const FUR: [u8; 3] = [255, 158, 196];
/// The inside of an ear, an eye and the nose: the neko panel's own dark.
const DARK: [u8; 3] = [34, 19, 32];

/// How many samples per axis each pixel is evaluated at.
///
/// **Three, not one.** At sixteen pixels a hard-edged circle with a triangle
/// on it is a stack of jagged steps, and the icon is drawn at sixteen more
/// often than at any other size -- that is what a taskbar and a title bar ask
/// for.
const SAMPLES: u32 = 3;

/// Draws the icon at `size` by `size`, as straight (non-premultiplied) RGBA.
///
/// Transparent outside the cat. An icon with its own opaque background is a
/// coloured square in a taskbar whatever the taskbar's colour is.
pub fn draw(size: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let step = 1.0 / (size as f32 * SAMPLES as f32);
    for y in 0..size {
        for x in 0..size {
            // Coverage and colour are accumulated together, so an edge where
            // fur meets an eye blends the two rather than blending fur with
            // nothing and painting the eye over it.
            let (mut cover, mut rgb) = (0.0f32, [0.0f32; 3]);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let u = (x as f32 * SAMPLES as f32 + sx as f32 + 0.5) * step;
                    let v = (y as f32 * SAMPLES as f32 + sy as f32 + 0.5) * step;
                    if let Some(colour) = sample(u, v) {
                        cover += 1.0;
                        for c in 0..3 {
                            rgb[c] += colour[c] as f32;
                        }
                    }
                }
            }
            let taken = (SAMPLES * SAMPLES) as f32;
            let at = ((y * size + x) * 4) as usize;
            if cover > 0.0 {
                for c in 0..3 {
                    pixels[at + c] = (rgb[c] / cover).round().clamp(0.0, 255.0) as u8;
                }
                pixels[at + 3] = (cover / taken * 255.0).round() as u8;
            }
        }
    }
    pixels
}

/// What is at one point of the unit square, or `None` for nothing.
fn sample(x: f32, y: f32) -> Option<[u8; 3]> {
    // Eyes and nose first: they sit on the head, so they win where they
    // overlap it. Testing them first is the same thing as painting them last
    // and costs no second pass.
    if in_circle(x, y, 0.385, 0.575, 0.062) || in_circle(x, y, 0.615, 0.575, 0.062) {
        return Some(DARK);
    }
    // The nose, a small triangle pointing down.
    if in_triangle(x, y, (0.455, 0.655), (0.545, 0.655), (0.5, 0.715)) {
        return Some(DARK);
    }
    // The inside of each ear.
    if in_triangle(x, y, (0.245, 0.365), (0.315, 0.135), (0.395, 0.335))
        || in_triangle(x, y, (0.755, 0.365), (0.685, 0.135), (0.605, 0.335))
    {
        return Some(DARK);
    }
    // Each ear, drawn under its own lining.
    if in_triangle(x, y, (0.195, 0.44), (0.295, 0.06), (0.44, 0.34))
        || in_triangle(x, y, (0.805, 0.44), (0.705, 0.06), (0.56, 0.34))
    {
        return Some(FUR);
    }
    // The head.
    if in_circle(x, y, 0.5, 0.58, 0.335) {
        return Some(FUR);
    }
    None
}

fn in_circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> bool {
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// Whether a point is inside a triangle, by the sign of three cross products.
///
/// Winding-independent -- it accepts a triangle given either way round --
/// because the two ears here are mirror images of each other and one of them
/// would otherwise have to be written backwards to be drawn at all.
fn in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let side = |p: (f32, f32), q: (f32, f32)| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
    let (u, v, w) = (side(a, b), side(b, c), side(c, a));
    (u >= 0.0 && v >= 0.0 && w >= 0.0) || (u <= 0.0 && v <= 0.0 && w <= 0.0)
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
