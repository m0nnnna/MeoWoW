//! The window's own icon.
//!
//! The executable's icon comes from the resource table and is attached by
//! `build.rs`; this is the one winit hands the compositor for the title bar
//! and, on the platforms that use it, the taskbar. Both draw the same cat from
//! [`crate::icon_art`].

use crate::icon_art;

/// How big the window icon is drawn.
///
/// One size, and 64 rather than 32: winit hands the platform a single bitmap
/// and lets it scale, and scaling down is the direction that survives.
const SIZE: u32 = 64;

/// Draws the icon, or `None` if winit will not take it.
///
/// **A failure here is a window with the default icon**, which is what a
/// window has always had, so it is logged and dropped rather than propagated.
/// There is no version of this worth failing a launch over.
pub fn window_icon() -> Option<winit::window::Icon> {
    match winit::window::Icon::from_rgba(icon_art::draw(SIZE), SIZE, SIZE) {
        Ok(icon) => Some(icon),
        Err(e) => {
            tracing::warn!("the window icon could not be built: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon has to *be* something. A drawing bug that produced an empty
    /// bitmap would show up as the default window icon, which is exactly what
    /// this feature looks like when it is switched off -- an absent capability
    /// and an absent thing producing the same picture, again.
    #[test]
    fn the_icon_is_not_blank() {
        let pixels = icon_art::draw(SIZE);
        assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
        let opaque = pixels.chunks_exact(4).filter(|p| p[3] > 200).count();
        let total = (SIZE * SIZE) as usize;
        // A cat's head on a transparent square: somewhere between a fifth and
        // three quarters of it is fur. Loose bounds on purpose -- this asserts
        // that something was drawn, not that it is pretty.
        assert!(
            opaque > total / 5 && opaque < total * 3 / 4,
            "{opaque} of {total} pixels are opaque"
        );
    }

    /// Two eyes and a nose in the middle, distinct from the fur around them.
    /// Without this the shape test passes on a plain pink circle.
    #[test]
    fn the_cat_has_a_face() {
        let pixels = icon_art::draw(SIZE);
        let at = |x: u32, y: u32| {
            let i = ((y * SIZE + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        let eye = at((SIZE as f32 * 0.385) as u32, (SIZE as f32 * 0.575) as u32);
        let cheek = at((SIZE as f32 * 0.5) as u32, (SIZE as f32 * 0.5) as u32);
        assert!(eye[3] > 200 && cheek[3] > 200, "both should be inside the head");
        assert!(
            (eye[0] as i32 - cheek[0] as i32).abs() > 100,
            "an eye must not be the same colour as the fur: {eye:?} against {cheek:?}"
        );
    }

    /// The corners are transparent, or the taskbar gets a pink square.
    #[test]
    fn the_icon_has_no_background() {
        let pixels = icon_art::draw(SIZE);
        for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
            let i = ((y * SIZE + x) * 4) as usize;
            assert_eq!(pixels[i + 3], 0, "corner {x},{y} is not transparent");
        }
    }

    /// The `.ico` container has to say how many images it holds and where each
    /// one starts. A wrong offset is an icon Windows silently declines to
    /// draw, which looks exactly like a build script that did not run.
    #[test]
    fn the_ico_directory_points_at_its_images() {
        let sizes = [16u32, 32, 48, 256];
        let file = icon_art::ico(&sizes);
        assert_eq!(&file[0..4], &[0, 0, 1, 0], "reserved, then type 1");
        assert_eq!(
            u16::from_le_bytes([file[4], file[5]]) as usize,
            sizes.len()
        );
        for (i, &size) in sizes.iter().enumerate() {
            let entry = 6 + i * 16;
            // 256 is stored as zero: one byte per dimension.
            let stored = if size >= 256 { 0 } else { size as u8 };
            assert_eq!(file[entry], stored, "width of the {size} entry");
            assert_eq!(file[entry + 1], stored, "height of the {size} entry");
            let length =
                u32::from_le_bytes(file[entry + 8..entry + 12].try_into().unwrap()) as usize;
            let offset =
                u32::from_le_bytes(file[entry + 12..entry + 16].try_into().unwrap()) as usize;
            assert!(
                offset + length <= file.len(),
                "the {size} image runs past the end of the file"
            );
            // Every image starts with a 40-byte BITMAPINFOHEADER whose height
            // is doubled to cover the mask -- the field that, written as the
            // true height, draws the icon's bottom half over its top.
            assert_eq!(
                u32::from_le_bytes(file[offset..offset + 4].try_into().unwrap()),
                40
            );
            assert_eq!(
                i32::from_le_bytes(file[offset + 8..offset + 12].try_into().unwrap()),
                size as i32 * 2,
                "the {size} entry's stored height"
            );
        }
    }
}
