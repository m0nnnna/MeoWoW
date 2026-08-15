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

pub mod camera;
pub mod edit;
pub mod element;
pub mod frames;
pub mod layout;
pub mod style;

use std::path::PathBuf;

pub use camera::Camera;
pub use edit::{EditAction, EditState};
pub use element::{Anchor, Element};
pub use frames::chat::{ChatEntry, ChatKind};
pub use frames::combat_text::{CombatTextKind, FloatingText};
pub use frames::{CastBarView, SpellbookEntry, UnitView};
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
    /// Damage numbers in flight, world-anchored like the target marker rather
    /// than placed by an [`Element`] -- see [`frames::combat_text`].
    pub combat_text: &'a [frames::combat_text::FloatingText],
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
    /// Everything the character can put on a bar, or `None` when the book is
    /// closed. Like the cast bar, "closed" is expressed by having nothing to
    /// draw rather than by a flag: the caller already decides when the book is
    /// open, and a second copy of that decision here could disagree with it.
    pub spellbook: Option<&'a [frames::SpellbookEntry]>,
    /// What the character is carrying, or `None` when the bag window is shut.
    /// Closed is expressed by having nothing to draw, exactly as it is for the
    /// spellbook and the cast bar.
    ///
    /// One flat list covering every slot the window shows, because this client
    /// draws **one** window rather than one per bag -- see [`frames::bags`].
    /// The caller decides which slots that is; this crate lays out however
    /// many it is given.
    pub bags: Option<&'a [frames::BagSlot]>,
    /// The nineteen worn slots, or `None` when the character panel is shut.
    ///
    /// A separate window from the bags rather than a section of it: the two
    /// answer different questions, and a worn slot has a fixed identity where
    /// a bag square is only a position.
    pub character: Option<&'a [frames::EquipSlot]>,
    /// The character's money in copper, drawn along the bottom of the bag
    /// window. Ignored when `bags` is `None`.
    pub copper: u32,
}

/// What the user did to the interface this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HudResponse {
    /// `(bar, slot)` of an action slot that was clicked with nothing held --
    /// the request to actually *use* what is in it.
    pub activated: Option<(usize, usize)>,
    /// Whether the layout changed in a way worth writing to disk: a spell put
    /// on a bar, or a slot cleared. Reported rather than saved here because
    /// this crate does the arranging and the caller owns when files get
    /// written -- and because a save on every frame of a drag would be a file
    /// write per frame.
    pub layout_changed: bool,
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
    /// The spell picked up out of the spellbook and not yet put down.
    ///
    /// Interface state rather than game state, so it lives here rather than
    /// being handed in each frame: picking a spell up and dropping it on a
    /// slot is entirely a thing that happens to the layout, and the layout is
    /// what this crate owns. The caller never has to know a drag is in
    /// progress.
    held: Option<u32>,
    /// The first spell shown in the book, as it is scrolled.
    spellbook_scroll: usize,
}

impl Default for Hud {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            edit: EditState::default(),
            path: None,
            status: None,
            occupied: Vec::new(),
            held: None,
            spellbook_scroll: 0,
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

        // Also drawn straight onto a layer and never added to `occupied`, for
        // the same reason as the target marker above: a damage number sits
        // over a creature, and claiming that rectangle for the interface
        // would make the creature underneath unclickable while it faded.
        if !data.combat_text.is_empty() {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("hud-combat-text"),
            ));
            frames::combat_text::draw(&painter, data.combat_text, &style);
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
            let spellbook_placeholder;
            let bags_placeholder;
            let character_placeholder;
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
                // Absent when closed, exactly like the cast bar -- and present
                // in edit mode regardless, or it could only be positioned
                // while open and would have to stay open for the whole drag.
                ElementId::Spellbook => match data.spellbook {
                    Some(entries) => Content::Spellbook(entries),
                    None if editing => {
                        spellbook_placeholder = frames::spellbook::placeholder();
                        Content::Spellbook(&spellbook_placeholder)
                    }
                    None => continue,
                },
                // Same rule as the spellbook: absent when shut, drawn in edit
                // mode so it can be positioned without a character logged in.
                ElementId::Bags => match data.bags {
                    Some(slots) => Content::Bags(slots),
                    None if editing => {
                        bags_placeholder = frames::bags::placeholder();
                        Content::Bags(&bags_placeholder)
                    }
                    None => continue,
                },
                ElementId::Character => match data.character {
                    Some(slots) => Content::Character(slots),
                    None if editing => {
                        character_placeholder = frames::character::placeholder();
                        Content::Character(&character_placeholder)
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
                Content::Spellbook(_) => frames::spellbook::size(&style, element.scale),
                // The only frame whose size depends on its contents: a
                // character with bags carries more than one without, and a
                // fixed height would either clip the grid or leave a band of
                // empty window under it.
                Content::Bags(slots) => frames::bags::size(slots.len(), &style, element.scale),
                Content::Character(_) => frames::character::size(&style, element.scale),
            };
            let rect = element.rect(screen, size);
            self.occupied.push(rect);

            // The book's wheel scrolling is answered here, before it is drawn,
            // and from `rect` rather than from egui's hover state. Both halves
            // are deliberate: reading the wheel after the frame is painted
            // would apply it a frame late, and the rectangle is already known,
            // so asking egui whether it thinks the panel is hovered would be
            // consulting a second opinion about a question this loop can
            // answer itself.
            //
            // Clamping happens every frame rather than only on a scroll,
            // because the entry list changes as the character learns things.
            // An offset left past the end shows a panel of blank rows, which
            // is indistinguishable from a book that failed to load.
            let scroll = match content {
                Content::Spellbook(entries) => {
                    let limit =
                        frames::spellbook::max_scroll(entries.len(), rect, &style, element.scale);
                    if !editing && limit > 0 {
                        if let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) {
                            if rect.contains(pointer) {
                                let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
                                // A positive wheel delta moves the content
                                // down, which means *earlier* in the list.
                                //
                                // Applied by saturating add and subtract
                                // rather than by casting the offset to a
                                // signed type and adding: the offset is a
                                // `usize`, and a large one casts to a negative
                                // number, which would silently scroll the
                                // wrong way instead of failing.
                                let rows = (wheel / (style.spellbook_row * element.scale)) as i32;
                                self.spellbook_scroll = if rows >= 0 {
                                    self.spellbook_scroll.saturating_sub(rows as usize)
                                } else {
                                    self.spellbook_scroll.saturating_add(rows.unsigned_abs() as usize)
                                };
                            }
                        }
                    }
                    self.spellbook_scroll = self.spellbook_scroll.min(limit);
                    self.spellbook_scroll
                }
                _ => 0,
            };
            let held = self.held;

            let response = egui::Area::new(egui::Id::new(("hud-element", id.key())))
                // Behind the debug and edit windows, which are ordinary egui
                // windows: the interface is the thing being worked on, not the
                // thing doing the working.
                .order(egui::Order::Background)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    let sense = if editing {
                        egui::Sense::drag()
                    } else if matches!(content, Content::Bar { .. } | Content::Spellbook(_)) {
                        // The two frames you interact with while playing, so
                        // they sense clicks rather than only hover.
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
                        Content::Spellbook(entries) => frames::spellbook::draw(
                            &painter,
                            response.rect,
                            entries,
                            scroll,
                            held,
                            &style,
                            element.scale,
                        ),
                        Content::Bags(slots) => frames::bags::draw(
                            &painter,
                            response.rect,
                            slots,
                            data.copper,
                            &style,
                            element.scale,
                        ),
                        Content::Character(slots) => frames::character::draw(
                            &painter,
                            response.rect,
                            slots,
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
            let drawn_rect = response.rect;
            match (editing, content) {
                (false, Content::Bar { index, slots }) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(slot) = frames::action_bar::slot_at(
                                drawn_rect,
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                match self.held.take() {
                                    // Holding a spell makes the click a *put*
                                    // rather than a use. Which of the two a
                                    // click means therefore depends on state
                                    // the user set a moment ago by clicking a
                                    // spell in the book -- and the held icon
                                    // follows the cursor precisely so that
                                    // state is never invisible.
                                    Some(spell) => {
                                        self.profile.bars.set(index, slot, Some(spell));
                                        response_out.layout_changed = true;
                                    }
                                    None => response_out.activated = Some((index, slot)),
                                }
                            }
                        }
                    }
                    // Right-click empties a slot. The only way to *remove*
                    // something without also putting something else there,
                    // and the alternative -- a modifier, or an edit-mode-only
                    // control -- would make clearing a slot a different kind
                    // of gesture from filling one.
                    if response.secondary_clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(slot) = frames::action_bar::slot_at(
                                drawn_rect,
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                if self.profile.bars.get(index, slot).is_some() {
                                    self.profile.bars.set(index, slot, None);
                                    response_out.layout_changed = true;
                                }
                            }
                        }
                    }
                    if let Some(pointer) = response.hover_pos() {
                        if let Some(slot) =
                            frames::action_bar::slot_at(drawn_rect, &style, element.scale, pointer)
                        {
                            if let Some(spell) = slots.get(slot).and_then(|s| s.spell.as_ref()) {
                                frames::action_bar::hover_tooltip(&response, spell);
                            }
                        }
                    }
                }
                (false, Content::Spellbook(entries)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(row) =
                                frames::spellbook::row_at(drawn_rect, &style, element.scale, pointer)
                            {
                                // The row is an index into what is *on
                                // screen*; the scroll offset turns it into an
                                // index into the book. Conflating the two is
                                // the bug this separation exists to prevent,
                                // and it only shows up once the book is long
                                // enough to scroll.
                                if let Some(entry) = entries.get(scroll + row) {
                                    self.held = Some(entry.id);
                                }
                            }
                        }
                    }
                }
                _ => {}
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

        // The spell being carried, drawn against the cursor.
        //
        // **A hold does not outlive the book.** The indicator is drawn from
        // the book's own entry, so a hold that survived the book closing would
        // be a mode with nothing on screen to show it -- and a click that
        // silently means "put" instead of "cast" is exactly the surprise this
        // interface should not have. Closing the book therefore puts the spell
        // back, as do Escape and a right-click anywhere.
        match (self.held, data.spellbook) {
            (Some(spell), Some(entries)) => {
                let dropped = ctx.input(|i| {
                    i.key_pressed(egui::Key::Escape) || i.pointer.secondary_clicked()
                });
                match entries.iter().find(|entry| entry.id == spell) {
                    Some(entry) if !dropped => {
                        if let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()) {
                            let painter = ctx.layer_painter(egui::LayerId::new(
                                egui::Order::Tooltip,
                                egui::Id::new("hud-held-spell"),
                            ));
                            frames::spellbook::draw_held(
                                &painter,
                                pointer,
                                entry,
                                &style,
                                self.profile.get(ElementId::Spellbook).scale,
                            );
                        }
                    }
                    // Dropped, or a spell the book no longer lists.
                    _ => self.held = None,
                }
            }
            (Some(_), None) => self.held = None,
            _ => {}
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
                        ElementId::Spellbook => frames::spellbook::size(&style, scale),
                        // Measured from what is actually being carried when
                        // there is anything, and from the placeholder's
                        // sixteen otherwise -- the same source the drawing
                        // loop used, so re-anchoring cannot move a frame it
                        // measured differently from how it painted it.
                        ElementId::Bags => frames::bags::size(
                            data.bags
                                .map(|slots| slots.len())
                                .unwrap_or_else(|| frames::bags::placeholder().len()),
                            &style,
                            scale,
                        ),
                        ElementId::Character => frames::character::size(&style, scale),
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
    Spellbook(&'a [frames::SpellbookEntry]),
    Bags(&'a [frames::BagSlot]),
    Character(&'a [frames::EquipSlot]),
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

    /// A damage number is drawn straight onto a layer like the target marker,
    /// not through an `Element` -- so this checks the same
    /// layout-arithmetic-versus-the-screen gap `a_cooldown_darkens_the_slot`
    /// watches for, applied to `Hud::show`'s other screen-space path: giving
    /// it an entry has to paint something, not merely compute one.
    #[test]
    fn combat_text_paints_something() {
        let entries = vec![frames::combat_text::FloatingText {
            pos: egui::pos2(400.0, 400.0),
            text: "6".into(),
            kind: frames::combat_text::CombatTextKind::Damage,
            age: 0.0,
        }];
        let mut hud = Hud::default();
        let rects = painted(
            &mut hud,
            &HudData {
                combat_text: &entries,
                ..Default::default()
            },
        );
        assert!(!rects.is_empty(), "a damage number painted nothing");
    }

    /// The number has to rise, not just fade: an older entry's painted shape
    /// must sit higher on screen (a smaller `top()`) than the same entry
    /// fresh, or the animation the style's `combat_text_rise` promises never
    /// actually reaches the screen.
    #[test]
    fn an_aged_combat_number_rises() {
        fn top_of(age: f32) -> f32 {
            let entries = vec![frames::combat_text::FloatingText {
                pos: egui::pos2(400.0, 400.0),
                text: "6".into(),
                kind: frames::combat_text::CombatTextKind::Damage,
                age,
            }];
            let mut hud = Hud::default();
            let rects = painted(
                &mut hud,
                &HudData {
                    combat_text: &entries,
                    ..Default::default()
                },
            );
            rects
                .iter()
                .map(|r| r.top())
                .fold(f32::MAX, f32::min)
        }

        let fresh = top_of(0.0);
        let aged = top_of(0.6);
        assert!(fresh < f32::MAX, "a fresh number painted nothing to measure");
        assert!(
            aged < fresh,
            "an older number must sit higher on screen: {aged} vs {fresh}"
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

    /// Drives the interface through real egui passes, one batch of events per
    /// pass, and returns the last [`HudResponse`].
    ///
    /// A click cannot be delivered in one pass: egui decides what a press
    /// landed on from the rectangles the *previous* pass registered, and
    /// reports `clicked()` on the release. So the script below is the whole of
    /// what a click is, spelled out -- and spelling it out is the point, since
    /// this is the harness that lets an assignment gesture be tested without a
    /// window.
    fn drive(hud: &mut Hud, data: &HudData<'_>, script: &[Vec<egui::Event>]) -> HudResponse {
        let ctx = egui::Context::default();
        let mut last = HudResponse::default();
        for events in script {
            let input = egui::RawInput {
                screen_rect: Some(screen()),
                events: events.clone(),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                last = hud.show(ui, data);
            });
            output.drop_without_applying_deltas();
        }
        last
    }

    /// One complete click at a point, as the passes it takes.
    fn click_script(pos: egui::Pos2, button: egui::PointerButton) -> Vec<Vec<egui::Event>> {
        let modifiers = egui::Modifiers::default();
        vec![
            vec![egui::Event::PointerMoved(pos)],
            vec![egui::Event::PointerMoved(pos)],
            vec![egui::Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers,
            }],
            vec![egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers,
            }],
        ]
    }

    /// Where the rows of the spellbook and the slots of the first action bar
    /// are on screen, given the default layout.
    fn spellbook_rows(profile: &Profile) -> Vec<egui::Pos2> {
        let element = profile.get(ElementId::Spellbook);
        let rect = element.rect(
            screen(),
            frames::spellbook::size(&profile.style, element.scale),
        );
        frames::spellbook::row_rects(rect, &profile.style, element.scale)
            .map(|row| row.center())
            .collect()
    }

    fn bar_slots(profile: &Profile) -> Vec<egui::Pos2> {
        let element = profile.get(ElementId::ActionBar1);
        let rect = element.rect(
            screen(),
            frames::action_bar::size(&profile.style, element.scale),
        );
        frames::action_bar::slot_rects(rect, &profile.style, element.scale)
            .map(|slot| slot.center())
            .collect()
    }

    fn book(count: usize) -> Vec<SpellbookEntry> {
        (0..count)
            .map(|i| SpellbookEntry {
                id: 100 + i as u32,
                name: format!("Spell {i}"),
                rank: String::new(),
                icon: None,
            })
            .collect()
    }

    /// The book is absent when closed and present in edit mode, the same
    /// asymmetry `a_cast_bar_appears_only_while_casting_or_editing` pins for
    /// the cast bar -- and for the same reason: it could otherwise only be
    /// positioned while open, and would have to stay open for the whole drag.
    #[test]
    fn a_spellbook_appears_only_when_open_or_editing() {
        let mut quiet = Hud::default();
        hide_bars(&mut quiet);
        assert!(
            painted(&mut quiet, &HudData::default()).is_empty(),
            "a spellbook was painted with the book closed"
        );

        let entries = book(4);
        let mut open = Hud::default();
        hide_bars(&mut open);
        assert!(
            !painted(
                &mut open,
                &HudData {
                    spellbook: Some(&entries),
                    ..Default::default()
                }
            )
            .is_empty(),
            "an open spellbook painted nothing"
        );
    }

    /// The same asymmetry for the bag window, and for the same reason.
    #[test]
    fn a_bag_window_appears_only_when_open_or_editing() {
        let mut quiet = Hud::default();
        hide_bars(&mut quiet);
        assert!(
            painted(&mut quiet, &HudData::default()).is_empty(),
            "a bag window was painted with the bags closed"
        );

        let slots = frames::bags::placeholder();
        let mut open = Hud::default();
        hide_bars(&mut open);
        assert!(
            !painted(
                &mut open,
                &HudData {
                    bags: Some(&slots),
                    ..Default::default()
                }
            )
            .is_empty(),
            "an open bag window painted nothing"
        );
    }

    /// **The check that a live-only bug is converted into a headless one.**
    ///
    /// Everything about the bag window that a person at a window would notice
    /// is a *number rendered as text*: the stack count in a slot's corner, the
    /// used-of-total in the header, and the money along the bottom. None of
    /// those is visible to a geometry assertion -- a window can paint the
    /// right rectangles in the right places while showing the wrong quantity
    /// of everything -- and all three read out of fields this milestone
    /// measured rather than transcribed, which is exactly the class of value
    /// this project's notes say is believed when wrong.
    ///
    /// So this asserts the text. The money is the number `.modify money`
    /// actually set on the live realm, so a regression in the split shows up
    /// as the same discrepancy a person would have reported.
    #[test]
    fn the_bag_window_says_what_it_is_carrying() {
        let mut slots = vec![frames::BagSlot::default(); 16];
        slots[0] = frames::BagSlot {
            item: Some(frames::BagItem {
                entry: 2589,
                name: "Linen Cloth".into(),
                count: 3,
                icon: None,
            }),
        };
        slots[1] = frames::BagSlot {
            item: Some(frames::BagItem {
                entry: 6948,
                name: "Hearthstone".into(),
                // A stack of one draws no number at all -- see `BagItem::count`.
                count: 1,
                icon: None,
            }),
        };

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let text = painted_text(&shapes(
            &mut hud,
            &HudData {
                bags: Some(&slots),
                copper: 123_456,
                ..Default::default()
            },
            None,
        ));

        assert!(text.contains(&"Bags".to_string()), "no title in {text:?}");
        assert!(
            text.contains(&"2/16".to_string()),
            "the window did not say how full it is: {text:?}"
        );
        assert!(
            text.contains(&"3".to_string()),
            "the stack of three lost its count: {text:?}"
        );
        assert!(
            text.contains(&"12g 34s 56c".to_string()),
            "the money was not drawn, or was split wrongly: {text:?}"
        );
        assert!(
            !text.contains(&"1".to_string()),
            "a stack of one drew a count it should have left off: {text:?}"
        );
    }

    /// The whole assignment gesture, end to end: click a spell, click a slot,
    /// and the layout holds it.
    ///
    /// This is the test the standing rule asks for -- the feature exists so a
    /// bar can be arranged in-game, and every part of that (which row was
    /// clicked, that a held spell turns a slot click into a put rather than a
    /// cast, that the layout is reported as changed so it gets saved) is
    /// invisible from outside and would otherwise only ever be checked by a
    /// person at a window.
    #[test]
    fn clicking_a_spell_then_a_slot_puts_it_on_the_bar() {
        let entries = book(4);
        let data = HudData {
            spellbook: Some(&entries),
            ..Default::default()
        };
        let profile = Profile::default();
        let rows = spellbook_rows(&profile);
        let slots = bar_slots(&profile);

        let mut hud = Hud::default();
        let mut script = click_script(rows[1], egui::PointerButton::Primary);
        script.extend(click_script(slots[3], egui::PointerButton::Primary));
        let response = drive(&mut hud, &data, &script);

        assert_eq!(
            hud.profile.bars.get(0, 3),
            Some(entries[1].id),
            "the second spell in the book did not land in the fourth slot"
        );
        assert!(
            response.layout_changed,
            "an assignment has to be reported, or it is never written to disk"
        );
        assert_eq!(
            response.activated, None,
            "a slot clicked while holding a spell must not also cast it"
        );
    }

    /// A slot with nothing held is still a cast, which is the behaviour that
    /// existed before assignment did and must not have been broken by it.
    #[test]
    fn a_slot_clicked_with_nothing_held_is_a_cast() {
        let mut hud = Hud::default();
        hud.profile.bars.set(0, 2, Some(78));
        let slots = bar_slots(&hud.profile);
        let response = drive(
            &mut hud,
            &HudData::default(),
            &click_script(slots[2], egui::PointerButton::Primary),
        );
        assert_eq!(response.activated, Some((0, 2)));
        assert!(!response.layout_changed);
        assert_eq!(hud.profile.bars.get(0, 2), Some(78), "casting emptied the slot");
    }

    /// A scrolled book has to pick up the spell *under the cursor*, not the
    /// one at that position in the list.
    ///
    /// The row index and the entry index are deliberately different things --
    /// see `frames::spellbook::row_at`. Conflating them is the obvious mistake
    /// here, and it is invisible until the book is long enough to scroll,
    /// which no short test and no first look at a new character would reach.
    #[test]
    fn a_scrolled_book_picks_up_the_spell_under_the_cursor() {
        let profile = Profile::default();
        let page = frames::spellbook::page_rows(
            &profile.style,
            profile.get(ElementId::Spellbook).scale,
        );
        let entries = book(page + 5);
        let data = HudData {
            spellbook: Some(&entries),
            ..Default::default()
        };
        let rows = spellbook_rows(&profile);
        let slots = bar_slots(&profile);

        let mut hud = Hud::default();
        // Scrolled past the end, which clamps to the last full page -- so the
        // first row on screen is entry number five rather than entry zero.
        // Deliberately `usize::MAX` rather than 5: an offset that large is
        // what the clamp exists for, and casting it to a signed type to apply
        // a wheel delta would turn it into -1 and scroll the other way.
        hud.spellbook_scroll = usize::MAX;
        let mut script = click_script(rows[0], egui::PointerButton::Primary);
        script.extend(click_script(slots[0], egui::PointerButton::Primary));
        drive(&mut hud, &data, &script);

        assert_eq!(
            hud.profile.bars.get(0, 0),
            Some(entries[5].id),
            "a scrolled book picked up the wrong spell"
        );
    }

    /// Right-clicking a slot is the only way to empty one without putting
    /// something else there.
    #[test]
    fn right_clicking_a_slot_empties_it() {
        let mut hud = Hud::default();
        hud.profile.bars.set(0, 5, Some(78));
        let slots = bar_slots(&hud.profile);
        let response = drive(
            &mut hud,
            &HudData::default(),
            &click_script(slots[5], egui::PointerButton::Secondary),
        );
        assert_eq!(hud.profile.bars.get(0, 5), None);
        assert!(response.layout_changed);
    }

    /// Closing the book puts down whatever was picked up.
    ///
    /// The held spell is drawn from the book's own entry, so a hold that
    /// outlived the book would be a mode with nothing on screen to show it --
    /// and the next click on a bar would silently mean "put" instead of
    /// "cast".
    #[test]
    fn closing_the_book_drops_what_was_held() {
        let entries = book(4);
        let profile = Profile::default();
        let rows = spellbook_rows(&profile);

        let mut hud = Hud::default();
        drive(
            &mut hud,
            &HudData {
                spellbook: Some(&entries),
                ..Default::default()
            },
            &click_script(rows[0], egui::PointerButton::Primary),
        );
        assert_eq!(hud.held, Some(entries[0].id), "the click picked nothing up");

        // One frame with the book closed.
        drive(&mut hud, &HudData::default(), &[vec![]]);
        assert_eq!(hud.held, None, "a hold outlived the book it came from");
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
            ElementId::Spellbook => frames::spellbook::size(&profile.style, scale),
            ElementId::Bags => {
                frames::bags::size(frames::bags::placeholder().len(), &profile.style, scale)
            }
            ElementId::Character => frames::character::size(&profile.style, scale),
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
