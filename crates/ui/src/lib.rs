//! The client's own interface.
//!
//! This is not a reimplementation of `FrameXML`. 3.3.5a's interface is Lua 5.1
//! driving an XML frame tree, and matching it closely enough to run addons
//! means reproducing that whole widget system -- including the parts nobody
//! would design today -- before the first health bar appears. This client draws
//! its own interface instead and gives up addon compatibility, and pays that
//! back by making the interface itself the thing that is configurable: every
//! position, size and colour lives in one text file the user owns, and can be
//! rearranged from inside the running client. See `docs/UI.md`.
//!
//! egui is the drawing and input substrate, nothing more. Frames are painted
//! from explicit geometry rather than assembled from egui widgets, so the
//! interface's appearance is [`Style`]'s to decide and `scale` genuinely
//! multiplies every dimension. What egui provides is a font atlas, a pointer,
//! and a tessellator -- the parts that would exist whatever the client drew.
//!
//! The pieces:
//!
//! - [`element`] -- where a frame sits: anchor, offset, scale, visibility.
//! - [`style`] -- what every frame draws with.
//! - [`layout`] -- the whole layout, and the file it lives in.
//! - [`frames`] -- the frames themselves.
//! - [`edit`] -- rearranging it all without leaving the world.
//! - [`Hud`] -- what a caller actually holds.

pub mod edit;
pub mod element;
pub mod frames;
pub mod layout;
pub mod style;

use std::path::PathBuf;

pub use edit::{EditAction, EditState};
pub use element::{Anchor, Element};
pub use frames::chat::{ChatEntry, ChatKind};
pub use frames::{CastBarView, UnitView};
pub use layout::{default_path, ElementId, Profile};
pub use style::{Color, PowerType, Style};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the layout file is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("the layout could not be written: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("no writable configuration directory: neither APPDATA nor HOME is set")]
    NoConfigDirectory,
}

/// What the interface needs to know each frame.
///
/// Borrowed rather than owned, and rebuilt by the caller every frame from
/// whatever it reads: this crate holds no game state of its own, so there is
/// nothing here that can go stale without the world going stale with it.
#[derive(Default)]
pub struct HudData<'a> {
    pub player: Option<&'a UnitView>,
    pub target: Option<&'a UnitView>,
    /// Where the selected unit is on screen, as the box the click was tested
    /// against -- see [`frames::marker`]. `None` when nothing is selected or
    /// the selection is behind the camera.
    pub target_marker: Option<egui::Rect>,
    /// The chat scrollback, oldest first. Owned and capped by the caller: this
    /// crate must not accumulate an unbounded log nobody drains.
    pub chat: &'a [frames::chat::ChatEntry],
    /// The line being typed, if the user is typing one.
    pub composing: Option<&'a str>,
    /// What each action bar is showing, resolved by the caller: this crate
    /// knows spell names and texture ids, never `Spell.dbc`.
    pub bars: &'a [Vec<frames::action_bar::SlotView>],
    /// The player's cast in progress, if there is one. `None` most of the
    /// time -- casting is the exception, not the steady state -- so the bar
    /// is absent outside edit mode exactly the way the target frame is
    /// absent with nothing targeted.
    pub cast_bar: Option<&'a frames::CastBarView>,
}

/// What the user did to the interface this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HudResponse {
    /// `(bar, slot)` of an action slot that was clicked.
    pub activated: Option<(usize, usize)>,
}

/// The interface, ready to draw.
pub struct Hud {
    pub profile: Profile,
    pub edit: EditState,
    /// Where the layout is read from and written to, if a configuration
    /// directory could be found at all.
    pub path: Option<PathBuf>,
    /// The last thing worth telling the user about the layout file -- a
    /// dropped element, a clamped value, a failed save. Held rather than only
    /// logged, because a customisation that did not take effect needs to say
    /// so where the customising is happening.
    pub status: Option<String>,
    /// Screen rectangles the interface drew into last frame, used by
    /// [`Hud::captures_pointer`].
    ///
    /// Kept rather than asking egui, because egui's own
    /// `is_pointer_over_egui` deliberately reports `false` for
    /// `Order::Background` layers that sit inside the root UI rect -- which is
    /// exactly what these frames are, and exactly the case that matters. Its
    /// answer is the right one for a debug overlay and the wrong one for an
    /// interface that is part of the game.
    occupied: Vec<egui::Rect>,
}

impl Default for Hud {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            edit: EditState::default(),
            path: None,
            status: None,
            occupied: Vec::new(),
        }
    }
}

impl Hud {
    /// Reads the user's layout, falling back to the default one.
    ///
    /// Deliberately infallible. A layout file is the one piece of state here
    /// that a user edits by hand, so it is the one most likely to be broken --
    /// and refusing to start a game client because a health bar's colour is
    /// misspelled would be a poor trade. Whatever went wrong lands in
    /// [`Hud::status`] and in the log, and the client starts.
    pub fn load() -> Self {
        let mut hud = Hud::default();
        let path = match default_path() {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!("{error}; the layout cannot be saved");
                hud.status = Some(error.to_string());
                return hud;
            }
        };
        hud.path = Some(path.clone());

        match Profile::load(&path) {
            Ok((profile, warnings)) => {
                hud.profile = profile;
                for warning in &warnings {
                    tracing::warn!("{}: {warning}", path.display());
                }
                if !warnings.is_empty() {
                    hud.status = Some(warnings.join("; "));
                }
                tracing::info!("loaded the interface layout from {}", path.display());
            }
            // Not an error, and not worth a warning: this is what every first
            // run looks like.
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("no layout at {}; using the defaults", path.display());
            }
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                hud.status = Some(format!("using the default layout: {error}"));
            }
        }
        hud
    }

    pub fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.status = Some("there is nowhere to save the layout".into());
            return;
        };
        match self.profile.save(&path) {
            Ok(()) => {
                tracing::info!("saved the interface layout to {}", path.display());
                self.status = Some(format!("saved to {}", path.display()));
            }
            Err(error) => {
                tracing::warn!("could not save {}: {error}", path.display());
                self.status = Some(format!("could not save: {error}"));
            }
        }
    }

    pub fn reload(&mut self) {
        let reloaded = Hud::load();
        self.profile = reloaded.profile;
        self.status = reloaded.status.or(Some("reloaded from disk".into()));
    }

    pub fn toggle_edit(&mut self) {
        self.edit.active = !self.edit.active;
    }

    /// Whether the pointer is over the interface, and so should not also be
    /// acting on the world behind it.
    ///
    /// Clicking a health bar must not target whatever creature happens to be
    /// standing behind it. The caller asks this before doing anything with a
    /// click, which keeps the question in one place instead of at every call
    /// site that might want it.
    ///
    /// Answered from the rectangles this crate drew last frame plus egui's own
    /// opinion about its windows -- see [`Hud::occupied`] for why the second
    /// alone is not enough.
    pub fn captures_pointer(&self, ctx: &egui::Context) -> bool {
        if ctx.egui_wants_pointer_input() {
            return true;
        }
        let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
            return false;
        };
        self.occupied.iter().any(|rect| rect.contains(pointer))
    }

    /// Draws the whole interface, and handles edit-mode dragging.
    pub fn show(&mut self, ctx: &egui::Context, data: &HudData<'_>) -> HudResponse {
        let mut response_out = HudResponse::default();
        let screen = ctx.content_rect();
        let style = self.profile.style;
        let editing = self.edit.active;
        self.occupied.clear();

        // Drawn straight onto a layer rather than inside an `Area`, and
        // deliberately never added to `occupied`. An Area would claim the
        // pointer over its own rectangle -- and this rectangle is drawn
        // *around a creature*, so claiming it would make the thing you just
        // selected the one thing you could no longer click.
        if let (true, Some(rect)) = (style.show_target_marker, data.target_marker) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("hud-target-marker"),
            ));
            frames::marker::draw(&painter, rect, &style);
        }

        for id in ElementId::ALL {
            let element = self.profile.get(id);
            if !element.visible {
                continue;
            }

            // In edit mode a frame with nothing to show still draws, filled
            // with plausible content. Otherwise the target frame could only be
            // positioned while something was targeted -- and it would have to
            // stay targeted for the whole drag.
            let unit_placeholder;
            let chat_placeholder;
            let cast_bar_placeholder;
            let bar_placeholder;
            let content = match id {
                ElementId::PlayerFrame | ElementId::TargetFrame => {
                    let live = if id == ElementId::PlayerFrame {
                        data.player
                    } else {
                        data.target
                    };
                    match live {
                        Some(unit) => Content::Unit(unit),
                        None if editing => {
                            unit_placeholder = UnitView::placeholder(id.label());
                            Content::Unit(&unit_placeholder)
                        }
                        None => continue,
                    }
                }
                ElementId::ChatFrame => {
                    if data.chat.is_empty() && data.composing.is_none() {
                        if !editing {
                            continue;
                        }
                        chat_placeholder = frames::chat::placeholder();
                        Content::Chat(&chat_placeholder)
                    } else {
                        Content::Chat(data.chat)
                    }
                }
                ElementId::CastBar => match data.cast_bar {
                    Some(view) => Content::CastBar(view),
                    None if editing => {
                        cast_bar_placeholder = frames::CastBarView::placeholder();
                        Content::CastBar(&cast_bar_placeholder)
                    }
                    None => continue,
                },
                _ => {
                    // An action bar. Unlike the other frames, an empty one
                    // still draws: the slots are where spells get *put*, so
                    // hiding them until something is on them would leave
                    // nowhere to put anything.
                    let index = id.action_bar().unwrap_or(0);
                    bar_placeholder = frames::action_bar::placeholder(index);
                    match data.bars.get(index) {
                        Some(slots) if !slots.is_empty() => Content::Bar { index, slots },
                        _ => Content::Bar {
                            index,
                            slots: &bar_placeholder,
                        },
                    }
                }
            };

            let size = match content {
                Content::Unit(unit) => frames::unit::size(&style, element.scale, unit.has_power()),
                Content::Chat(_) => frames::chat::size(&style, element.scale),
                Content::Bar { .. } => frames::action_bar::size(&style, element.scale),
                Content::CastBar(_) => frames::cast_bar::size(&style, element.scale),
            };
            let rect = element.rect(screen, size);
            self.occupied.push(rect);
            let response = egui::Area::new(egui::Id::new(("hud-element", id.key())))
                // Behind the debug and edit windows, which are ordinary egui
                // windows: the interface is the thing being worked on, not the
                // thing doing the working.
                .order(egui::Order::Background)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    let sense = if editing {
                        egui::Sense::drag()
                    } else if matches!(content, Content::Bar { .. }) {
                        // A bar is the one frame you interact with while
                        // playing, so it senses clicks rather than only hover.
                        egui::Sense::click()
                    } else {
                        // Still sensed, so `captures_pointer` knows the
                        // pointer is over the interface even when nothing here
                        // is draggable.
                        egui::Sense::hover()
                    };
                    let (response, painter) = ui.allocate_painter(size, sense);
                    match content {
                        Content::Unit(unit) => {
                            frames::unit::draw(&painter, response.rect, unit, &style, element.scale)
                        }
                        Content::Chat(lines) => frames::chat::draw(
                            &painter,
                            response.rect,
                            lines,
                            data.composing,
                            &style,
                            element.scale,
                        ),
                        Content::Bar { slots, .. } => frames::action_bar::draw(
                            &painter,
                            response.rect,
                            slots,
                            &style,
                            element.scale,
                        ),
                        Content::CastBar(view) => frames::cast_bar::draw(
                            &painter,
                            response.rect,
                            view,
                            &style,
                            element.scale,
                        ),
                    }
                    if editing {
                        paint_edit_chrome(&painter, response.rect, id, &style, element.scale);
                    }
                    response
                })
                .inner;

            // Clicking a slot casts, and hovering one explains what is in
            // it. Both read the same geometry the slots were drawn with, so
            // a click or a tooltip cannot disagree about where slot seven
            // actually is -- which means `response.rect`, the rectangle
            // `draw` was handed, and not the `rect` the layout asked for.
            // The two are equal today; they would stop being equal the moment
            // egui constrained the area, and the failure then is a click that
            // casts the neighbouring spell.
            let bar_rect = response.rect;
            if let (false, Content::Bar { index, slots }) = (editing, content) {
                if response.clicked() {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        if let Some(slot) =
                            frames::action_bar::slot_at(bar_rect, &style, element.scale, pointer)
                        {
                            response_out.activated = Some((index, slot));
                        }
                    }
                }
                if let Some(pointer) = response.hover_pos() {
                    if let Some(slot) =
                        frames::action_bar::slot_at(bar_rect, &style, element.scale, pointer)
                    {
                        if let Some(spell) = slots.get(slot).and_then(|s| s.spell.as_ref()) {
                            frames::action_bar::hover_tooltip(&response, spell);
                        }
                    }
                }
            }

            if editing {
                if response.hovered() || response.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                }
                if response.dragged() {
                    let moved = rect.min + response.drag_delta();
                    let mut element = element;
                    element.offset = element.offset_for(screen, size, moved);
                    if self.edit.snap {
                        element.offset = edit::snapped(element.offset, self.edit.grid);
                    }
                    self.profile.set(id, element);
                }
            }
        }

        if editing {
            // The window needs each frame's size to re-anchor without moving
            // it, and a frame's size depends on what it is drawing -- so the
            // same has-power question the loop above answered is answered
            // again here, from the same data. Measured up front rather than on
            // demand: the window holds the profile mutably while it runs, so a
            // closure that read sizes out of the profile could not also be
            // handed to it.
            let sizes: Vec<(ElementId, egui::Vec2)> = ElementId::ALL
                .into_iter()
                .map(|id| {
                    let scale = self.profile.get(id).scale;
                    let size = match id {
                        ElementId::ChatFrame => frames::chat::size(&style, scale),
                        ElementId::CastBar => frames::cast_bar::size(&style, scale),
                        ElementId::ActionBar1 | ElementId::ActionBar2 | ElementId::ActionBar3 => {
                            frames::action_bar::size(&style, scale)
                        }
                        ElementId::PlayerFrame | ElementId::TargetFrame => {
                            let unit = if id == ElementId::PlayerFrame {
                                data.player
                            } else {
                                data.target
                            };
                            let has_power = unit.map(|u| u.has_power()).unwrap_or(true);
                            frames::unit::size(&style, scale, has_power)
                        }
                    };
                    (id, size)
                })
                .collect();
            let size_of = move |id: ElementId| {
                sizes
                    .iter()
                    .find(|(candidate, _)| *candidate == id)
                    .map(|(_, size)| *size)
                    .unwrap_or_default()
            };
            let action = edit::window(
                ctx,
                &mut self.profile,
                &mut self.edit,
                self.path.as_deref(),
                &size_of,
            );
            match action {
                EditAction::Save => self.save(),
                EditAction::Reload => self.reload(),
                EditAction::ResetAll => {
                    self.profile.reset();
                    self.status = Some("reset to the default layout".into());
                }
                EditAction::None => {}
            }
        }

        response_out
    }
}

/// What one element is drawing this frame.
///
/// Borrowed from [`HudData`], or from a placeholder when edit mode needs
/// something to put in an otherwise empty frame.
enum Content<'a> {
    Unit(&'a UnitView),
    Chat(&'a [frames::chat::ChatEntry]),
    Bar {
        index: usize,
        slots: &'a [frames::action_bar::SlotView],
    },
    CastBar(&'a frames::CastBarView),
}

/// The outline and label that mark a frame as draggable.
fn paint_edit_chrome(
    painter: &egui::Painter,
    rect: egui::Rect,
    id: ElementId,
    style: &Style,
    scale: f32,
) {
    let colour: egui::Color32 = style.edit_highlight.into();
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same((style.corner * scale).round().clamp(0.0, 255.0) as u8),
        egui::Stroke::new(1.5, colour),
        egui::StrokeKind::Outside,
    );
    painter.text(
        rect.left_bottom() + egui::vec2(0.0, 2.0),
        egui::Align2::LEFT_TOP,
        id.label(),
        egui::FontId::proportional(11.0),
        colour,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: egui::Vec2 = egui::vec2(1600.0, 900.0);

    fn screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)
    }

    /// Runs the interface through a headless egui context and reports every
    /// rectangle it actually painted.
    ///
    /// Two passes, because egui settles a pass behind itself for anything it
    /// has to measure, and a test that read the first pass would be asserting
    /// against a half-built frame.
    fn painted(hud: &mut Hud, data: &HudData<'_>) -> Vec<egui::Rect> {
        shapes(hud, data, None)
            .iter()
            .map(|clipped| clipped.shape.visual_bounding_rect())
            .filter(|rect| rect.is_positive())
            .collect()
    }

    /// The shapes themselves, optionally with the pointer resting somewhere.
    ///
    /// The pointer is delivered as a real `PointerMoved` on every pass rather
    /// than as a bare `RawInput::pointer` field, because egui decides what is
    /// hovered from the event stream -- and the interface asks `hover_pos()`
    /// which slot the cursor is on.
    fn shapes(
        hud: &mut Hud,
        data: &HudData<'_>,
        pointer: Option<egui::Pos2>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(screen()),
            events: pointer.map(egui::Event::PointerMoved).into_iter().collect(),
            ..Default::default()
        };
        // Two passes for the interface itself, plus two more when a pointer is
        // involved: hovering is only known once a pass has registered where the
        // widgets are, and a tooltip's first pass is egui's invisible sizing
        // pass. Reading any earlier reports a tooltip that is genuinely drawn
        // as missing.
        let passes = if pointer.is_some() { 4 } else { 2 };
        let mut shapes = Vec::new();
        for _ in 0..passes {
            let mut output = ctx.run_ui(input.clone(), |ui| {
                hud.show(ui, data);
            });
            shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
        }
        shapes
    }

    /// Every string that reached the screen, tooltips included.
    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    fn player() -> UnitView {
        UnitView::placeholder("Testwolf")
    }

    /// The layout arithmetic being right is not the same as anything reaching
    /// the screen.
    ///
    /// This project has already lost time to exactly that gap in the renderer:
    /// geometry submitted at zero size looks identical to geometry never
    /// submitted, and the search went to the wrong layer for it. So this asks
    /// the question the other way round -- run the real `show`, and check that
    /// something was painted, at the rectangle the layout chose.
    #[test]
    fn a_frame_paints_at_the_rectangle_the_layout_chose() {
        let mut hud = Hud::default();
        let unit = player();
        let expected = {
            let element = hud.profile.get(ElementId::PlayerFrame);
            element.rect(
                screen(),
                frames::unit::size(&hud.profile.style, element.scale, unit.has_power()),
            )
        };

        let rects = painted(
            &mut hud,
            &HudData {
                player: Some(&unit),
                ..Default::default()
            },
        );
        assert!(
            rects.iter().any(|rect| rect.contains_rect(expected)
                || (rect.min - expected.min).length() < 1.0),
            "nothing was painted at {expected:?}; got {rects:?}"
        );
    }

    /// And the other half of that question: an element that is switched off
    /// must paint nothing at all, not something invisible.
    #[test]
    fn a_hidden_frame_paints_nothing() {
        let unit = player();
        let data = HudData {
            player: Some(&unit),
            ..Default::default()
        };

        let mut shown = Hud::default();
        let before = painted(&mut shown, &data).len();

        let mut hidden = Hud::default();
        hidden.profile.edit(ElementId::PlayerFrame).visible = false;
        let after = painted(&mut hidden, &data).len();

        assert!(before > 0, "the visible case painted nothing to compare to");
        assert!(
            after < before,
            "hiding the player frame changed nothing: {before} shapes either way"
        );
    }

    /// A unit frame with nothing to show is absent, not empty -- but in edit
    /// mode it appears anyway, or the target frame could only be positioned
    /// while something was targeted, and would have to stay targeted for the
    /// whole drag.
    ///
    /// Measured against the *bar-free* case, because an action bar
    /// deliberately draws while empty: its slots are where spells get put, so
    /// hiding them until something is on them would leave nowhere to put
    /// anything. That asymmetry is intentional and this test pins it.
    #[test]
    fn unit_frames_appear_without_data_only_while_editing() {
        let empty = HudData::default();

        let mut quiet = Hud::default();
        hide_bars(&mut quiet);
        assert!(
            painted(&mut quiet, &empty).is_empty(),
            "unit frames were painted with nothing to put in them"
        );

        let mut editing = Hud::default();
        hide_bars(&mut editing);
        editing.edit.active = true;
        assert!(
            !painted(&mut editing, &empty).is_empty(),
            "edit mode has nothing to drag"
        );
    }

    /// An action bar draws even with nothing on it, unlike every other frame.
    #[test]
    fn an_empty_action_bar_still_draws() {
        let mut hud = Hud::default();
        assert!(
            !painted(&mut hud, &HudData::default()).is_empty(),
            "an empty bar left nowhere to put a spell"
        );
    }

    /// A slot mid-cooldown paints the darkening sweep on top of its icon or
    /// text, so a spell on cooldown must paint more shapes than the same slot
    /// with nothing remaining -- the layout-arithmetic-versus-the-screen gap
    /// this crate's tests already watch for, applied to the sweep specifically.
    #[test]
    fn a_cooldown_darkens_the_slot() {
        fn bars_with_cooldown(fraction: f32) -> Vec<Vec<frames::action_bar::SlotView>> {
            let mut slots = frames::action_bar::placeholder(0);
            slots[0].spell = Some(frames::action_bar::SlotSpell {
                id: 78,
                name: "Heroic Strike".into(),
                rank: String::new(),
                description: String::new(),
                icon: None,
                cooldown_fraction: fraction,
            });
            vec![slots]
        }

        let mut ready = Hud::default();
        let ready_shapes = painted(
            &mut ready,
            &HudData {
                bars: &bars_with_cooldown(0.0),
                ..Default::default()
            },
        )
        .len();

        let mut on_cooldown = Hud::default();
        let cooldown_shapes = painted(
            &mut on_cooldown,
            &HudData {
                bars: &bars_with_cooldown(0.6),
                ..Default::default()
            },
        )
        .len();

        assert!(
            cooldown_shapes > ready_shapes,
            "the cooldown sweep painted no extra shape: {cooldown_shapes} vs {ready_shapes}"
        );
    }

    /// A cast bar with nothing to show is absent, not empty -- but in edit
    /// mode it appears anyway, the same asymmetry `unit_frames_appear_without_data_only_while_editing`
    /// already pins for the unit frames, applied to the one other frame that
    /// is absent by default rather than always drawn like an action bar.
    #[test]
    fn a_cast_bar_appears_only_while_casting_or_editing() {
        let empty = HudData::default();

        // The action bars draw even while empty by design (see
        // `an_empty_action_bar_still_draws`), so they are hidden here the
        // same way `unit_frames_appear_without_data_only_while_editing` hides
        // them: otherwise their shapes would swamp the question this test is
        // actually asking.
        let mut quiet = Hud::default();
        hide_bars(&mut quiet);
        assert!(
            painted(&mut quiet, &empty).is_empty(),
            "a cast bar was painted with nothing being cast"
        );

        let mut editing = Hud::default();
        hide_bars(&mut editing);
        editing.edit.active = true;
        assert!(
            !painted(&mut editing, &empty).is_empty(),
            "edit mode has nothing to drag for the cast bar"
        );
    }

    /// A cast bar mid-cast paints its fill on top of the backdrop, so a cast
    /// with real progress must paint more shapes than the same bar at
    /// `0.0` -- the same layout-arithmetic-versus-the-screen gap
    /// `a_cooldown_darkens_the_slot` already watches for, applied to the fill
    /// rather than the sweep.
    #[test]
    fn a_cast_bar_fills_as_the_cast_progresses() {
        fn cast(progress: f32) -> frames::CastBarView {
            frames::CastBarView {
                spell_name: "Healing Touch".into(),
                progress,
                cast_time_ms: 3000,
            }
        }

        let mut starting = Hud::default();
        let started = cast(0.0);
        let starting_shapes = painted(
            &mut starting,
            &HudData {
                cast_bar: Some(&started),
                ..Default::default()
            },
        )
        .len();

        let mut midway = Hud::default();
        let progressed = cast(0.6);
        let midway_shapes = painted(
            &mut midway,
            &HudData {
                cast_bar: Some(&progressed),
                ..Default::default()
            },
        )
        .len();

        assert!(
            midway_shapes > starting_shapes,
            "the cast bar's fill painted no extra shape: {midway_shapes} vs {starting_shapes}"
        );
    }

    /// Hovering a slot has to put the spell's *full* name on the screen, which
    /// the slot itself never does -- an icon shows no text at all and the
    /// fallback shows an abbreviation. So "Heroic Strike" appearing anywhere
    /// is proof the tooltip was painted and not merely computed.
    ///
    /// Worth a headless test rather than another live look, because the reason
    /// the tooltip exists is that two slots can be pixel-identical
    /// (`Activate Primary Spec` and `Activate Secondary Spec` share
    /// `spell_icon_id` 2970), and a tooltip that silently stopped appearing
    /// would leave exactly the ambiguity it was added to remove.
    #[test]
    fn a_hovered_slot_explains_itself() {
        let mut slots = frames::action_bar::placeholder(0);
        slots[0].spell = Some(frames::action_bar::SlotSpell {
            id: 78,
            name: "Heroic Strike".into(),
            rank: "Rank 1".into(),
            description: "A strong attack.".into(),
            icon: None,
            cooldown_fraction: 0.0,
        });
        let bars = vec![slots];
        let data = HudData {
            bars: &bars,
            ..Default::default()
        };

        let profile = Profile::default();
        let element = profile.get(ElementId::ActionBar1);
        let rect = element.rect(
            screen(),
            frames::action_bar::size(&profile.style, element.scale),
        );
        let centres: Vec<egui::Pos2> =
            frames::action_bar::slot_rects(rect, &profile.style, element.scale)
                .map(|slot| slot.center())
                .collect();

        let mut hud = Hud::default();
        let filled = painted_text(&shapes(&mut hud, &data, Some(centres[0])));
        let mut hud = Hud::default();
        // The same widget, hovered where nothing is assigned: any difference
        // is the tooltip and nothing else.
        let empty = painted_text(&shapes(&mut hud, &data, Some(centres[1])));

        for wanted in ["Heroic Strike", "Rank 1", "A strong attack."] {
            assert!(
                filled.iter().any(|text| text == wanted),
                "hovering a filled slot never painted {wanted:?}; got {filled:?}"
            );
            assert!(
                !empty.iter().any(|text| text == wanted),
                "hovering an empty slot painted {wanted:?} anyway"
            );
        }
    }

    fn hide_bars(hud: &mut Hud) {
        for id in ElementId::ALL {
            if id.action_bar().is_some() {
                hud.profile.edit(id).visible = false;
            }
        }
    }

    /// How big an element is, matching what `show` does. Used by the layout
    /// tests below, which would otherwise measure an action bar as if it were
    /// a unit frame.
    fn size_of(profile: &Profile, id: ElementId) -> egui::Vec2 {
        let scale = profile.get(id).scale;
        match id {
            ElementId::ChatFrame => frames::chat::size(&profile.style, scale),
            ElementId::CastBar => frames::cast_bar::size(&profile.style, scale),
            ElementId::ActionBar1 | ElementId::ActionBar2 | ElementId::ActionBar3 => {
                frames::action_bar::size(&profile.style, scale)
            }
            ElementId::PlayerFrame | ElementId::TargetFrame => {
                frames::unit::size(&profile.style, scale, true)
            }
        }
    }

    /// The chat box is the first frame here that has to wrap text, so "it
    /// painted" and "it painted the lines" are different claims. This checks
    /// the second: more lines have to produce more painted shapes.
    #[test]
    fn chat_paints_a_shape_per_line() {
        let one = [ChatEntry {
            kind: ChatKind::Say,
            who: Some("Testwolf".into()),
            text: "hello".into(),
            prefix: None,
        }];
        let many: Vec<ChatEntry> = (0..5)
            .map(|i| ChatEntry {
                kind: ChatKind::Say,
                who: Some("Testwolf".into()),
                text: format!("line {i}"),
                prefix: None,
            })
            .collect();

        let mut hud = Hud::default();
        let few = painted(
            &mut hud,
            &HudData {
                chat: &one,
                ..Default::default()
            },
        )
        .len();
        let mut hud = Hud::default();
        let lots = painted(
            &mut hud,
            &HudData {
                chat: &many,
                ..Default::default()
            },
        )
        .len();

        assert!(few > 0, "an empty chat box painted nothing at all");
        assert!(lots > few, "five lines painted no more than one: {lots} vs {few}");
    }

    /// The scrollback grows downward, so a box too small for its history has
    /// to lose the *oldest* lines, not the newest. Losing the newest would be
    /// the one failure that makes chat useless while looking like it works.
    #[test]
    fn an_overfull_chat_box_keeps_the_newest_lines() {
        let many: Vec<ChatEntry> = (0..200)
            .map(|i| ChatEntry {
                kind: ChatKind::Say,
                who: None,
                text: format!("line {i}"),
                prefix: None,
            })
            .collect();

        let mut hud = Hud::default();
        let style = hud.profile.style;
        let element = hud.profile.get(ElementId::ChatFrame);
        let box_rect = element.rect(screen(), frames::chat::size(&style, element.scale));

        let rects = painted(
            &mut hud,
            &HudData {
                chat: &many,
                ..Default::default()
            },
        );
        // Every painted line must lie inside the box; nothing may run off the
        // top drawing history nobody asked for.
        let strays = rects
            .iter()
            .filter(|rect| rect.bottom() < box_rect.top() - 1.0)
            .count();
        assert_eq!(strays, 0, "lines were painted above the chat box");
    }

    /// A layout that cannot be read must not stop the client. The fallback is
    /// the default profile, and the reason lands in `status` rather than in an
    /// `Err` nobody is in a position to handle at startup.
    #[test]
    fn a_broken_layout_falls_back_rather_than_failing() {
        let hud = Hud {
            profile: Profile::from_toml("scale = ")
                .map(|(p, _)| p)
                .unwrap_or_default(),
            ..Default::default()
        };
        assert_eq!(hud.profile, Profile::default());
    }

    /// The default layout has to be usable on the smallest screen anyone would
    /// run this on, or a first-time user's frames are off the edge with no
    /// visible way to drag them back.
    #[test]
    fn the_default_frames_fit_a_small_window() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
        let profile = Profile::default();
        for id in ElementId::ALL {
            let element = profile.get(id);
            let rect = element.rect(screen, size_of(&profile, id));
            assert!(
                screen.contains_rect(rect),
                "{} lands at {rect:?}, outside {screen:?}",
                id.label()
            );
        }
    }

    /// And they must not sit on top of each other, which a shared default
    /// offset would quietly produce.
    #[test]
    fn the_default_frames_do_not_overlap() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let profile = Profile::default();
        // Only what is actually shown. The two modifier bars default to
        // hidden and sit deliberately where the first one would be if it grew,
        // so overlapping while invisible is not a fault.
        let shown: Vec<(ElementId, egui::Rect)> = ElementId::ALL
            .into_iter()
            .filter(|id| profile.get(*id).visible)
            .map(|id| (id, profile.get(id).rect(screen, size_of(&profile, id))))
            .collect();
        for (i, (a_id, a)) in shown.iter().enumerate() {
            for (b_id, b) in &shown[i + 1..] {
                assert!(
                    !a.intersects(*b),
                    "{} overlaps {}",
                    a_id.label(),
                    b_id.label()
                );
            }
        }
    }
}
