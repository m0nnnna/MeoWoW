//! Rearranging the interface from inside the running client.
//!
//! The layout file is the source of truth and can be edited by hand, but a
//! text file is a poor way to answer "is that health bar too big?" -- the loop
//! is edit, restart, look, repeat. Edit mode closes that loop: the same
//! [`crate::Profile`] is dragged around on screen and written back to the same
//! file, so neither route is the "real" one.

use egui::{Context, Vec2};

use crate::element::Anchor;
use crate::layout::{ElementId, Profile};

/// Whether the layout is being rearranged, and how.
#[derive(Clone, Copy, Debug)]
pub struct EditState {
    pub active: bool,
    /// Rounds dragged offsets to whole multiples of [`EditState::grid`].
    pub snap: bool,
    pub grid: f32,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            active: false,
            snap: true,
            // Small enough to feel like free movement, large enough that two
            // frames dragged to "the same" margin actually share one.
            grid: 4.0,
        }
    }
}

/// What the edit window asked for, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    None,
    Save,
    Reload,
    ResetAll,
}

/// Rounds an offset onto the grid.
pub fn snapped(offset: [f32; 2], grid: f32) -> [f32; 2] {
    if !(grid > 0.0) {
        return offset;
    }
    [round_to(offset[0], grid), round_to(offset[1], grid)]
}

fn round_to(value: f32, grid: f32) -> f32 {
    (value / grid).round() * grid
}

/// The layout window: everything edit mode can do that dragging cannot.
///
/// `size_of` is supplied by the caller rather than computed here, because a
/// frame's size depends on what it is drawing (a creature with no mana gets a
/// shorter frame) and this module deliberately knows nothing about that.
/// Re-anchoring needs the size to hold the frame still, so it has to be asked
/// for rather than assumed.
pub(crate) fn window(
    ctx: &Context,
    profile: &mut Profile,
    state: &mut EditState,
    path: Option<&std::path::Path>,
    size_of: &dyn Fn(ElementId) -> Vec2,
) -> EditAction {
    let screen = ctx.content_rect();
    let mut action = EditAction::None;

    egui::Window::new("Interface")
        .default_width(320.0)
        .show(ctx, |ui| {
            ui.label("Drag any frame to move it.");
            ui.horizontal(|ui| {
                ui.checkbox(&mut state.snap, "snap to grid");
                ui.add_enabled(
                    state.snap,
                    egui::DragValue::new(&mut state.grid).range(1.0..=64.0),
                );
            });
            ui.separator();

            for id in ElementId::ALL {
                let size = size_of(id);
                ui.push_id(id.key(), |ui| {
                    ui.collapsing(id.label(), |ui| {
                        let element = profile.edit(id);
                        ui.checkbox(&mut element.visible, "visible");

                        let mut chosen = element.anchor;
                        ui.horizontal(|ui| {
                            ui.label("anchor");
                            egui::ComboBox::from_id_salt("anchor")
                                .selected_text(chosen.label())
                                .show_ui(ui, |ui| {
                                    for anchor in Anchor::ALL {
                                        ui.selectable_value(&mut chosen, anchor, anchor.label());
                                    }
                                });
                        });
                        // Re-anchoring must not move the frame -- see
                        // `Element::rebase`.
                        if chosen != element.anchor {
                            element.rebase(chosen, screen, size);
                        }

                        ui.horizontal(|ui| {
                            ui.label("offset");
                            ui.add(egui::DragValue::new(&mut element.offset[0]).speed(1.0));
                            ui.add(egui::DragValue::new(&mut element.offset[1]).speed(1.0));
                        });
                        ui.add(
                            egui::Slider::new(
                                &mut element.scale,
                                crate::element::MIN_SCALE..=crate::element::MAX_SCALE,
                            )
                            .text("scale"),
                        );
                        if ui.button("reset this frame").clicked() {
                            profile.reset_element(id);
                        }
                    });
                });
            }

            ui.separator();
            ui.label("Style");
            let style = &mut profile.style;
            ui.add(egui::Slider::new(&mut style.frame_width, 100.0..=600.0).text("frame width"));
            ui.add(egui::Slider::new(&mut style.bar_height, 6.0..=48.0).text("bar height"));
            ui.add(egui::Slider::new(&mut style.font_size, 8.0..=32.0).text("font size"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut style.show_values, "values");
                ui.checkbox(&mut style.show_percent, "percent");
            });
            colour_row(ui, "health", &mut style.health);
            colour_row(ui, "background", &mut style.background);
            colour_row(ui, "text", &mut style.text);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    action = EditAction::Save;
                }
                if ui.button("Reload").clicked() {
                    action = EditAction::Reload;
                }
                if ui.button("Reset all").clicked() {
                    action = EditAction::ResetAll;
                }
            });
            // Shown because the window exposes a chosen few settings and the
            // file exposes all of them; a user who wants the rest needs to
            // know where to look without going hunting.
            match path {
                Some(path) => {
                    ui.weak(format!("every setting lives in {}", path.display()));
                }
                None => {
                    ui.weak("no writable configuration directory; changes cannot be saved");
                }
            }
        });

    action
}

/// A colour swatch that writes back through our own [`crate::style::Color`],
/// which is what the layout file stores.
fn colour_row(ui: &mut egui::Ui, label: &str, colour: &mut crate::style::Color) {
    let mut rgba = colour.0;
    ui.horizontal(|ui| {
        if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
            *colour = crate::style::Color(rgba);
        }
        ui.label(label);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapping has to be a projection: dragging a frame that is already on
    /// the grid must not nudge it, or a held drag walks the frame across the
    /// screen one grid step per frame.
    #[test]
    fn snapping_is_idempotent() {
        let once = snapped([37.0, -19.0], 4.0);
        assert_eq!(snapped(once, 4.0), once);
        assert_eq!(once, [36.0, -20.0]);
    }

    /// And it must never move a frame further than half a cell, or the frame
    /// visibly disagrees with the pointer.
    #[test]
    fn snapping_never_moves_further_than_half_a_cell() {
        let grid = 8.0;
        for x in -50..50 {
            let value = x as f32 * 1.7;
            let moved = (snapped([value, 0.0], grid)[0] - value).abs();
            assert!(moved <= grid / 2.0 + f32::EPSILON, "{value} moved {moved}");
        }
    }

    /// A grid of zero would divide by it, and a negative one would invert the
    /// rounding; both come straight from a `DragValue` a user can type into.
    #[test]
    fn a_degenerate_grid_leaves_the_offset_alone() {
        assert_eq!(snapped([3.5, -7.5], 0.0), [3.5, -7.5]);
        assert_eq!(snapped([3.5, -7.5], f32::NAN), [3.5, -7.5]);
    }
}
