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
pub use frames::{
    CastBarView, InviteAnswer, PartyInviteView, PartyMemberView, QuestDetail, QuestLogEntry,
    QuestgiverAction, QuestgiverClick, QuestgiverRow,
    MapMarker, MapPatch, MapView, MarkerKind, QuestgiverView, SpellbookEntry, UnitView,
};
pub use layout::{default_path, CharacterBars, ElementId, Profile};
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
    /// Where the character's own current corpse is on screen, world-anchored
    /// like `target_marker` above rather than placed by an [`Element`].
    /// `None` while alive, while a ghost's query has not yet been answered,
    /// or while the corpse is behind the camera.
    pub corpse_marker: Option<egui::Rect>,
    /// The `!` and `?` over questgivers, each with the screen box of the unit
    /// it belongs to. World-anchored like the two markers above, and resolved
    /// by the caller: which mark an NPC gets is the *server's* answer to
    /// `CMSG_QUESTGIVER_STATUS_QUERY`, not something this crate could work
    /// out.
    pub quest_marks: &'a [(egui::Rect, frames::QuestMark)],
    /// A lootable corpse's screen box, one per corpse -- world-anchored like
    /// `quest_marks`, and resolved the same way: the caller already knows
    /// which entities are lootable (`world::state::Entity::lootable`) and
    /// where they project to, so this crate only draws boxes it is handed.
    pub loot_markers: &'a [egui::Rect],
    /// Seconds on the caller's own running clock, for the sparkle's pulse.
    /// This crate never reads a clock itself -- see [`Self::bars`]'
    /// `cooldown_fraction` for the same rule applied to a bar icon -- so the
    /// one animation here that genuinely has no state to be a fraction *of*
    /// still takes the time as a plain number instead.
    pub loot_sparkle_time: f32,
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
    /// The release-spirit prompt, or `None` when the character is alive or is
    /// already a ghost. Like `loot`, existence *is* the flag: there is
    /// nothing else here that could disagree about whether it should be on
    /// screen.
    pub release_prompt: Option<&'a frames::ReleasePromptView>,
    /// What is on the corpse currently open, or `None` when none is.
    ///
    /// Unlike every other window here this one is not toggled by a key -- it
    /// appears because the server answered a loot request and goes away when
    /// the corpse is released. "Open" is therefore expressed entirely by
    /// having something to draw, with no flag anywhere that could disagree.
    pub loot: Option<&'a [frames::LootRow]>,
    /// The quest log, or `None` when the panel is shut. Existence is the flag,
    /// as it is for the spellbook and the bag window.
    ///
    /// The caller resolves each quest's title and objective out of its own
    /// cache; this crate knows nothing about `CMSG_QUEST_QUERY` and never
    /// decides that a quest has no objectives -- see
    /// [`frames::quest_log::QuestDetail`], which makes "not answered yet" a
    /// state a row can be in rather than an empty string.
    pub quest_log: Option<&'a [frames::QuestLogEntry]>,
    /// Which quest is highlighted in the log, if any.
    pub selected_quest: Option<u32>,
    /// The open conversation with a questgiver, or `None`. Existence is the
    /// flag, as it is for the loot window: the caller already decides when a
    /// conversation is open and a second copy here could disagree.
    pub questgiver: Option<&'a frames::QuestgiverView>,
    /// The world map, or `None` when it is shut. Existence is the flag, as it
    /// is for the spellbook and the bag window.
    ///
    /// The caller has already turned the player's position and every quest
    /// marker into fractions of the page -- this crate does not own the
    /// projection and must not, because a second copy of it would agree with
    /// `dbc::worldmap` right up until one of them changed.
    pub world_map: Option<&'a frames::MapView>,
    /// The character's money in copper, drawn along the bottom of the bag
    /// window. Ignored when `bags` is `None`.
    pub copper: u32,
    /// Everyone in the group but the player, or an empty slice when there is
    /// no group. **Emptiness is the flag**, as it is for the loot window: the
    /// caller already decides who is in the party and a second copy of that
    /// decision here could disagree with it.
    ///
    /// The player's own frame is deliberately not in this list -- the server
    /// leaves the recipient out of `SMSG_GROUP_LIST`, and this client already
    /// draws them in a frame of their own.
    pub party: &'a [frames::PartyMemberView],
    /// A pending group invite, or `None`. Existence is the flag again.
    ///
    /// **State rather than an event**, unlike a chat line: an invite stays
    /// open until it is answered or times out, so a caller that treated it as
    /// something that arrived once would flash a prompt for a single frame.
    pub party_invite: Option<&'a frames::PartyInviteView>,
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
    /// A loot row the user clicked, as *what to ask the server for* rather
    /// than as a position. See [`frames::loot`]: a row index and a loot slot
    /// are different numbers, and the difference is invisible until a corpse
    /// has been partly looted.
    pub take_loot: Option<frames::Take>,
    /// The release-spirit prompt was clicked. Carried as a bare flag rather
    /// than a guid or a slot -- unlike a loot row there is nothing to choose
    /// between, the whole frame is the one thing it can ask for.
    pub release_clicked: bool,
    /// A bag-window drag completed: `(picked up from, put down on)`, both as
    /// *positions in the list this crate was handed* -- see `frames::bags`.
    /// Never the destination alone, the same reasoning [`Self::take_loot`]
    /// carries a slot rather than a row: this crate has no idea which (bag,
    /// slot) pair a row corresponds to, only the caller that built the list
    /// does.
    pub move_item: Option<(usize, usize)>,
    /// A bag slot was right-clicked with nothing held. Same row-position
    /// caveat as [`Self::move_item`].
    ///
    /// **The gesture, not the action.** Right-click means "activate this",
    /// and what that *is* depends on the item: equip a sword, drink the
    /// water, go home on the hearthstone. Only the caller can decide, because
    /// only the caller has the server's answer about what the item does --
    /// this crate is handed a name, a count and an icon.
    ///
    /// This field was called `auto_equip`, and the name quietly decided the
    /// question: every right-click became an equip request, so a hearthstone
    /// was offered to the equipment slots, refused, and appeared to do
    /// nothing at all.
    pub activate_item: Option<usize>,
    /// A quest log row was clicked, reported as the **quest id** rather than
    /// as the row's position. Same reasoning as [`Self::take_loot`] carrying a
    /// loot slot: a row number means nothing outside the list this crate was
    /// handed, and the list is rebuilt whenever the log changes.
    pub selected_quest: Option<u32>,
    /// What was pressed in the questgiver window.
    pub questgiver: frames::QuestgiverClick,
    /// A party row was clicked, reported as the member's **guid** rather than
    /// as the row's position -- the same reasoning as [`Self::selected_quest`]
    /// carrying a quest id. The list is rebuilt from every group list the
    /// server sends, and the server resends it in full whenever anything
    /// changes, so a position is meaningless by the time the caller reads it.
    pub party_target: Option<u64>,
    /// How the player answered a group invite, or `None` if they have not.
    ///
    /// An enum rather than a `bool`, because the two answers travel by
    /// different opcodes and a caller reading `true` as "accept" is one
    /// inverted condition away from declining every invite silently.
    pub party_invite: Option<frames::InviteAnswer>,
}

/// What the cursor is currently carrying, picked up from either window that
/// offers something to drag.
///
/// One enum rather than the two independent `Option`s it replaces, so a
/// spell and an item can never both be held at once -- a state nothing here
/// could draw or make sense of a drop for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Spell(u32),
    /// A position in the bag-window list the caller was handed, not a real
    /// `(bag, slot)` pair -- see [`HudResponse::move_item`].
    Item(usize),
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
    /// Whose bars are in play, so every save files them under the right name.
    ///
    /// **Kept here rather than passed to `save`** because there are several
    /// save paths -- a spell dropped on a bar, a frame dragged, the window
    /// closing -- and one of them forgetting the name would write a
    /// character's bars into another's the next time they logged in. The rule
    /// this project keeps relearning: when a fact is easy to forget, attach it
    /// to the thing that needs it rather than to the call that usually has it.
    character: Option<String>,
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
    /// The thing picked up out of the spellbook or the bag window and not yet
    /// put down.
    ///
    /// Interface state rather than game state, so it lives here rather than
    /// being handed in each frame: picking something up and dropping it on a
    /// slot is entirely a thing that happens to the layout, and the layout is
    /// what this crate owns. The caller never has to know a drag is in
    /// progress. One field rather than two, because a hold is a single mode
    /// the cursor is in -- a `(spell, item)` pair of options would let both
    /// be `Some` at once, a state nothing here could make sense of.
    held: Option<Held>,
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
            character: None,
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

    /// Puts a character's own action bars in play.
    ///
    /// Everything after this -- arranging a bar, saving the file -- happens to
    /// that character's set. See [`Profile::use_character`] for what happens
    /// when the file has never seen this character before.
    pub fn use_character(&mut self, name: &str, knows: &dyn Fn(u32) -> bool) -> CharacterBars {
        self.character = Some(name.to_string());
        self.profile.use_character(name, knows)
    }

    pub fn save(&mut self) {
        // Filed under the character before anything is written, on every save
        // path there is. Skipped when no character is logged in, so a layout
        // edited from the asset viewer does not invent one.
        if let Some(name) = self.character.clone() {
            self.profile.remember_character(&name);
        }
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

    /// How many rectangles the interface drew into last frame. For a caller
    /// reporting why a click went where it did -- an interface that claims
    /// nothing and one whose handler ignored a click look identical from
    /// outside.
    pub fn occupied_count(&self) -> usize {
        self.occupied.len()
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
            frames::marker::draw(&painter, rect, style.target_marker, style.target_marker_width);
        }

        // Same shape as the selection bracket above, and the same reasoning
        // for staying off the `occupied` list: this sits over the player's
        // own corpse out in the world, and claiming that rectangle would make
        // the body itself unclickable underneath the interface.
        if let (true, Some(rect)) = (style.show_corpse_marker, data.corpse_marker) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("hud-corpse-marker"),
            ));
            frames::marker::draw(&painter, rect, style.corpse_marker, style.target_marker_width);
        }

        // The exclamations and question marks over questgivers. Same layer
        // treatment and the same reason: a mark floats over a creature, and
        // claiming its rectangle for the interface would make the creature
        // underneath unclickable -- which for a questgiver would mean the mark
        // saying "talk to me" was the thing preventing it.
        if style.show_quest_marks && !data.quest_marks.is_empty() {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("hud-quest-marks"),
            ));
            for (rect, mark) in data.quest_marks {
                frames::quest_mark::draw(
                    &painter,
                    *rect,
                    *mark,
                    style.quest_mark_bright.into(),
                    style.quest_mark_dim.into(),
                    style.quest_mark_size,
                );
            }
        }

        // Same layer treatment as the quest marks just above, and the same
        // reason: a sparkle sits over a corpse, and claiming its rectangle
        // for the interface would make the corpse underneath unclickable --
        // which here would mean the shine saying "loot me" was the thing
        // stopping you from doing it.
        if style.show_loot_sparkle && !data.loot_markers.is_empty() {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("hud-loot-sparkle"),
            ));
            for rect in data.loot_markers {
                frames::loot_sparkle::draw(
                    &painter,
                    *rect,
                    data.loot_sparkle_time,
                    style.loot_sparkle.into(),
                    style.loot_sparkle_size,
                );
            }
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

        // **Bottom of the interface first**, so a window that wants an answer
        // is never sealed under one that was only being read. See
        // [`ElementId::stacking`] -- egui stacks same-order areas by the order
        // they are built, which makes this loop's sequence the z-order.
        for id in ElementId::in_draw_order() {
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
            let loot_placeholder;
            let quest_log_placeholder;
            let questgiver_placeholder;
            let world_map_placeholder;
            let release_prompt_placeholder;
            let party_placeholder;
            let party_invite_placeholder;
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
                ElementId::Loot => match data.loot {
                    Some(rows) => Content::Loot(rows),
                    None if editing => {
                        loot_placeholder = frames::loot::placeholder();
                        Content::Loot(&loot_placeholder)
                    }
                    None => continue,
                },
                ElementId::QuestLog => match data.quest_log {
                    Some(entries) => Content::QuestLog(entries),
                    None if editing => {
                        quest_log_placeholder = frames::quest_log::placeholder();
                        Content::QuestLog(&quest_log_placeholder)
                    }
                    None => continue,
                },
                ElementId::Questgiver => match data.questgiver {
                    Some(view) => Content::Questgiver(view),
                    None if editing => {
                        questgiver_placeholder = frames::questgiver::placeholder();
                        Content::Questgiver(&questgiver_placeholder)
                    }
                    None => continue,
                },
                // Same rule again: shut means absent, and edit mode draws it
                // so the largest window in the interface can be positioned
                // without standing in a zone that has a page.
                ElementId::WorldMap => match data.world_map {
                    Some(view) => Content::WorldMap(view),
                    None if editing => {
                        world_map_placeholder = frames::world_map::placeholder();
                        Content::WorldMap(&world_map_placeholder)
                    }
                    None => continue,
                },
                // Absent while alive or already a ghost, on the same reasoning
                // as the loot window: existence is the flag, and drawn in
                // edit mode so it can be positioned without dying first.
                ElementId::ReleasePrompt => match data.release_prompt {
                    Some(view) => Content::ReleasePrompt(view),
                    None if editing => {
                        release_prompt_placeholder = frames::ReleasePromptView::placeholder();
                        Content::ReleasePrompt(&release_prompt_placeholder)
                    }
                    None => continue,
                },
                // Absent with nobody else in the group, which is the state
                // a character spends most of its life in -- so emptiness is
                // the flag, exactly as it is for the loot window. Drawn in
                // edit mode regardless, because the alternative is that the
                // frame can only be positioned while two other people are
                // logged in and stay in the group for the length of a drag.
                ElementId::PartyFrame => {
                    if data.party.is_empty() {
                        if !editing {
                            continue;
                        }
                        party_placeholder = frames::PartyMemberView::placeholder();
                        Content::Party(&party_placeholder)
                    } else {
                        Content::Party(data.party)
                    }
                }
                ElementId::PartyInvite => match data.party_invite {
                    Some(view) => Content::PartyInvite(view),
                    None if editing => {
                        party_invite_placeholder = frames::PartyInviteView::placeholder();
                        Content::PartyInvite(&party_invite_placeholder)
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
                // Sized to the corpse, so a one-item corpse does not open a
                // window with empty lines in it.
                Content::Loot(rows) => frames::loot::size(rows.len(), &style, element.scale),
                // Sized to the log, so a character on one quest does not open
                // a panel with twenty-four blank lines.
                Content::QuestLog(entries) => {
                    frames::quest_log::size(entries, &style, element.scale)
                }
                // Sized to the quest's text, which runs to paragraphs -- a
                // fixed height would clip the longest ones, and they are the
                // ones worth reading.
                Content::Questgiver(view) => {
                    frames::questgiver::size(view, &style, element.scale)
                }
                // The one frame whose size ignores its contents entirely:
                // the page's shape is fixed by the art, not by what is on it.
                Content::WorldMap(_) => frames::world_map::size(&style, element.scale),
                Content::ReleasePrompt(_) => frames::release::size(&style, element.scale),
                // Sized to the party *and* to what is known about each member
                // -- a member out of view has no power bar, so the rows are
                // not all the same height. See `frames::party::size`.
                Content::Party(members) => frames::party::size(members, &style, element.scale),
                Content::PartyInvite(_) => frames::party_invite::size(&style, element.scale),
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
            // The spellbook only cares whether a *spell* is held, to grey
            // its own entry out while it is being dragged -- a held bag item
            // means nothing to it.
            let held = match self.held {
                Some(Held::Spell(id)) => Some(id),
                _ => None,
            };

            // Which egui layer this frame belongs in, and whether it has to be
            // pinned to the top of it. See [`ElementId::layer`]: the loop's
            // sequence alone does *not* decide the z-order, because egui keeps
            // one per layer that outlives the frame.
            let (order, pinned) = id.layer();
            let layer = egui::LayerId::new(order, egui::Id::new(("hud-element", id.key())));
            let response = egui::Area::new(layer.id)
                // Below the debug and edit windows, which are ordinary egui
                // windows moved up to `Order::Foreground`: the interface is the
                // thing being worked on, not the thing doing the working.
                .order(order)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    let sense = if editing {
                        egui::Sense::drag()
                    } else if matches!(
                        content,
                        Content::Bar { .. }
                            | Content::Spellbook(_)
                            | Content::Loot(_)
                            | Content::QuestLog(_)
                            | Content::Questgiver(_)
                            | Content::ReleasePrompt(_)
                            | Content::Bags(_)
                            | Content::Party(_)
                            | Content::PartyInvite(_)
                    ) {
                        // The frames you interact with while playing, so they
                        // sense clicks rather than only hover.
                        //
                        // **This list is easy to forget and fails silently.**
                        // A frame left out of it draws correctly, hit-tests
                        // correctly, and simply never reports a click --
                        // `response.clicked()` is never true, so the match arm
                        // handling it is dead code that looks alive. The loot
                        // window shipped that way for exactly one live test:
                        // it opened, drew its rows, and did nothing when they
                        // were clicked. Anything that reads `response.clicked()`
                        // below must appear here.
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
                        Content::Loot(rows) => frames::loot::draw(
                            &painter,
                            response.rect,
                            rows,
                            &style,
                            element.scale,
                        ),
                        Content::QuestLog(entries) => frames::quest_log::draw(
                            &painter,
                            response.rect,
                            entries,
                            data.selected_quest,
                            &style,
                            element.scale,
                        ),
                        Content::Questgiver(view) => frames::questgiver::draw(
                            &painter,
                            response.rect,
                            view,
                            &style,
                            element.scale,
                        ),
                        Content::WorldMap(view) => frames::world_map::draw(
                            &painter,
                            response.rect,
                            view,
                            &style,
                            element.scale,
                        ),
                        Content::ReleasePrompt(view) => frames::release::draw(
                            &painter,
                            response.rect,
                            view,
                            &style,
                            element.scale,
                        ),
                        Content::Party(members) => frames::party::draw(
                            &painter,
                            response.rect,
                            members,
                            &style,
                            element.scale,
                        ),
                        Content::PartyInvite(view) => frames::party_invite::draw(
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

            // **Asked for every frame, not only when it opens.** egui moves an
            // area to the top of its layer when it is clicked or when it
            // appears, and both of those would otherwise leave a lower-ranked
            // frame above a higher-ranked one for as long as it stayed open --
            // press an action bar that a loot window overlaps and the bar
            // would come out in front of the window it is covering. Asking
            // every frame costs a set insertion and settles the question in
            // one direction only: egui's end-of-frame sort is stable, so
            // pinned frames keep their own order among themselves and merely
            // sit above the unpinned ones.
            if pinned {
                ctx.move_to_top(layer);
            }

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
                                    Some(Held::Spell(spell)) => {
                                        self.profile.bars.set(index, slot, Some(spell));
                                        response_out.layout_changed = true;
                                    }
                                    // An item has nowhere to go on an action
                                    // bar. Restored rather than dropped, so a
                                    // stray click on the wrong frame does not
                                    // silently cancel a drag.
                                    Some(other @ Held::Item(_)) => self.held = Some(other),
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
                (false, Content::Loot(rows)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(row) = frames::loot::row_at(
                                drawn_rect,
                                rows.len(),
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                // **The row carries what to ask for; this
                                // does not derive it.** A loot slot is the
                                // server's index into that corpse, and a
                                // corpse whose earlier slots are gone still
                                // numbers the rest from where they were, so
                                // reporting the row number would take the
                                // wrong item -- silently, since nothing
                                // acknowledges the request.
                                response_out.take_loot = rows.get(row).map(|row| row.take);
                            }
                        }
                    }
                }
                (false, Content::Questgiver(view)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            // One call answers rows and buttons both, so the
                            // drawing and the hit test cannot disagree about
                            // which the window is currently showing.
                            response_out.questgiver = frames::questgiver::click_at(
                                drawn_rect,
                                view,
                                &style,
                                element.scale,
                                pointer,
                            );
                        }
                    }
                }
                (false, Content::QuestLog(entries)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(row) = frames::quest_log::entry_at(
                                drawn_rect,
                                entries,
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                // The quest **id**, not the row. The list is
                                // rebuilt whenever the log changes, so a
                                // position is meaningless by the time the
                                // caller reads it.
                                response_out.selected_quest =
                                    entries.get(row).map(|entry| entry.id);
                            }
                        }
                    }
                }
                // Hover only, and no click: a pin says which quest it belongs
                // to and nothing here can act on it yet. It is deliberately
                // *not* in the `Sense::click()` list above, so nothing reads
                // a click that would never arrive.
                (false, Content::WorldMap(view)) => {
                    if let Some(pointer) = response.hover_pos() {
                        if let Some(index) = frames::world_map::marker_at(
                            drawn_rect,
                            view,
                            &style,
                            element.scale,
                            pointer,
                        ) {
                            if let Some(marker) = view.markers.get(index) {
                                frames::world_map::hover_tooltip(&response, &marker.label);
                            }
                        }
                    }
                }
                (false, Content::Party(members)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(row) = frames::party::row_at(
                                drawn_rect,
                                members,
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                // The member's **guid**, not the row. See
                                // `HudResponse::party_target`: the list is
                                // rebuilt from every group list the server
                                // sends, and it resends the whole group
                                // whenever anything about it changes.
                                response_out.party_target =
                                    members.get(row).map(|member| member.guid);
                            }
                        }
                    }
                }
                // The one frame here with *two* opposite answers, which is why
                // a press that lands between them reports nothing rather than
                // the nearer of the two. See `frames::party_invite::click_at`.
                (false, Content::PartyInvite(_)) => {
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            response_out.party_invite = frames::party_invite::click_at(
                                drawn_rect,
                                &style,
                                element.scale,
                                pointer,
                            );
                        }
                    }
                }
                // No row geometry to test -- the whole rectangle is the one
                // thing this frame can ask for.
                (false, Content::ReleasePrompt(_)) => {
                    if response.clicked() {
                        response_out.release_clicked = true;
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
                                    self.held = Some(Held::Spell(entry.id));
                                }
                            }
                        }
                    }
                }
                // The same click-to-pick-up, click-to-place gesture as the
                // spellbook above, on `self.held` rather than a second
                // mechanism -- see `Held`. `row` is a position in the flat
                // list this crate was handed, never a real `(bag, slot)`
                // pair; the caller resolves it, the same way a loot row is
                // not a loot slot.
                (false, Content::Bags(slots)) => {
                    if response.clicked() {
                        // **Logged because the silent branches here are the
                        // whole difficulty of `foss-wow#79`.** "I clicked an
                        // item and nothing happened" has four causes that look
                        // identical from the far side of the screen: the frame
                        // never got the click, it got one with no pointer
                        // position, the position mapped to no square, or it
                        // mapped to an empty square. Only the first is visible
                        // in the press router's own line, and the rest were
                        // invisible everywhere -- so a report could not be
                        // told apart from a report about the opposite bug.
                        let pointer = response.interact_pointer_pos();
                        let landed = pointer.and_then(|pointer| {
                            frames::bags::slot_at(
                                drawn_rect,
                                slots.len(),
                                &style,
                                element.scale,
                                pointer,
                            )
                        });
                        tracing::debug!(
                            "bags: click at {pointer:?} over {drawn_rect:?} -> slot {landed:?} \
                             of {}, holding {:?}",
                            slots.len(),
                            self.held
                        );
                        if let Some(pointer) = pointer {
                            if let Some(row) = frames::bags::slot_at(
                                drawn_rect,
                                slots.len(),
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                match self.held.take() {
                                    Some(Held::Item(from)) if from != row => {
                                        response_out.move_item = Some((from, row));
                                    }
                                    // Clicked back on the slot it came from,
                                    // or nothing was there to move: put it
                                    // down where it started rather than
                                    // reporting a move to itself.
                                    Some(Held::Item(_)) => {}
                                    // A spell has nowhere to go in a bag
                                    // window. Restored rather than dropped,
                                    // the same reasoning the action bar's
                                    // arm above uses.
                                    Some(other @ Held::Spell(_)) => self.held = Some(other),
                                    None => {
                                        if slots.get(row).is_some_and(|s| s.item.is_some()) {
                                            self.held = Some(Held::Item(row));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Right-click auto-equips instead of picking up, and only
                    // with nothing already held -- a right-click mid-drag has
                    // no obvious meaning and this does not invent one.
                    if response.secondary_clicked() && self.held.is_none() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if let Some(row) = frames::bags::slot_at(
                                drawn_rect,
                                slots.len(),
                                &style,
                                element.scale,
                                pointer,
                            ) {
                                if slots.get(row).is_some_and(|s| s.item.is_some()) {
                                    response_out.activate_item = Some(row);
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

        // The thing being carried, drawn against the cursor.
        //
        // **A hold does not outlive the window it came from.** The indicator
        // is drawn from that window's own current data, so a hold that
        // survived the window closing would be a mode with nothing on screen
        // to show it -- and a click that silently means "put" instead of
        // "cast" (or "move" instead of "auto-equip") is exactly the surprise
        // this interface should not have. Closing the window therefore puts
        // the thing back, as do Escape and a right-click anywhere.
        let dropped =
            ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.pointer.secondary_clicked());
        match (self.held, data.spellbook, data.bags) {
            (Some(Held::Spell(spell)), Some(entries), _) => {
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
            (Some(Held::Spell(_)), None, _) => self.held = None,
            (Some(Held::Item(row)), _, Some(slots)) => {
                match slots.get(row).and_then(|s| s.item.as_ref()) {
                    Some(item) if !dropped => {
                        if let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()) {
                            let painter = ctx.layer_painter(egui::LayerId::new(
                                egui::Order::Tooltip,
                                egui::Id::new("hud-held-item"),
                            ));
                            frames::bags::draw_held(
                                &painter,
                                pointer,
                                item,
                                &style,
                                self.profile.get(ElementId::Bags).scale,
                            );
                        }
                    }
                    // Dropped, or the slot no longer holds what it did --
                    // the server answered while it was held, say.
                    _ => self.held = None,
                }
            }
            (Some(Held::Item(_)), _, None) => self.held = None,
            (None, ..) => {}
        }

        if editing {
            // The window needs each frame's size to re-anchor without moving
            // it, and a frame's size depends on what it is drawing -- so the
            // same has-power question the loop above answered is answered
            // again here, from the same data. Measured up front rather than on
            // demand: the window holds the profile mutably while it runs, so a
            // closure that read sizes out of the profile could not also be
            // handed to it.
            // Same source the drawing loop used for its own placeholder, so
            // re-anchoring cannot measure a frame differently from how it was
            // painted.
            let edit_quest_log = frames::quest_log::placeholder();
            let edit_questgiver = frames::questgiver::placeholder();
            let edit_party = frames::PartyMemberView::placeholder();
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
                        ElementId::QuestLog => {
                            frames::quest_log::size(
                                data.quest_log.unwrap_or(&edit_quest_log),
                                &style,
                                scale,
                            )
                        }
                        ElementId::Questgiver => frames::questgiver::size(
                            data.questgiver.unwrap_or(&edit_questgiver),
                            &style,
                            scale,
                        ),
                        ElementId::WorldMap => frames::world_map::size(&style, scale),
                        ElementId::Loot => frames::loot::size(
                            data.loot
                                .map(|rows| rows.len())
                                .unwrap_or_else(|| frames::loot::placeholder().len()),
                            &style,
                            scale,
                        ),
                        ElementId::ReleasePrompt => frames::release::size(&style, scale),
                        // Same rule as the bags and the loot window: the real
                        // party where there is one, the placeholder where
                        // there is not, so re-anchoring measures the frame
                        // the loop above actually painted.
                        ElementId::PartyFrame => frames::party::size(
                            if data.party.is_empty() {
                                &edit_party
                            } else {
                                data.party
                            },
                            &style,
                            scale,
                        ),
                        ElementId::PartyInvite => frames::party_invite::size(&style, scale),
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
    Loot(&'a [frames::LootRow]),
    QuestLog(&'a [frames::QuestLogEntry]),
    Questgiver(&'a frames::QuestgiverView),
    WorldMap(&'a frames::MapView),
    ReleasePrompt(&'a frames::ReleasePromptView),
    Party(&'a [frames::PartyMemberView]),
    PartyInvite(&'a frames::PartyInviteView),
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
                press_fraction: 0.0,
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

    /// A just-pressed slot paints an extra shape (the flash ring) the same
    /// way a cooldown paints an extra one -- see `a_cooldown_darkens_the_slot`.
    /// This is the property that would have caught `foss-wow#74`: a click
    /// that reaches the slot but is never drawn is indistinguishable from a
    /// dropped keypress, and this is the check that it is not dropped.
    #[test]
    fn a_press_flashes_the_slot() {
        fn bars_with_press(fraction: f32) -> Vec<Vec<frames::action_bar::SlotView>> {
            let mut slots = frames::action_bar::placeholder(0);
            slots[0].spell = Some(frames::action_bar::SlotSpell {
                id: 78,
                name: "Heroic Strike".into(),
                rank: String::new(),
                description: String::new(),
                icon: None,
                cooldown_fraction: 0.0,
                press_fraction: fraction,
            });
            vec![slots]
        }

        let mut idle = Hud::default();
        let idle_shapes = painted(
            &mut idle,
            &HudData {
                bars: &bars_with_press(0.0),
                ..Default::default()
            },
        )
        .len();

        let mut pressed = Hud::default();
        let pressed_shapes = painted(
            &mut pressed,
            &HudData {
                bars: &bars_with_press(1.0),
                ..Default::default()
            },
        )
        .len();

        assert!(
            pressed_shapes > idle_shapes,
            "the press flash painted no extra shape: {pressed_shapes} vs {idle_shapes}"
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
            press_fraction: 0.0,
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

    /// One pass through the real event loop, against a context the caller
    /// keeps.
    ///
    /// [`drive`] makes a fresh context per script, which is exactly what hides
    /// a stacking bug: every area is then new in the same pass and egui leaves
    /// them in the order they were built. A test about *which window is on
    /// top* has to open them at different moments, and so has to hold the
    /// context across the passes itself.
    fn pass(
        ctx: &egui::Context,
        hud: &mut Hud,
        data: &HudData<'_>,
        events: Vec<egui::Event>,
    ) -> HudResponse {
        let input = egui::RawInput {
            screen_rect: Some(screen()),
            events,
            ..Default::default()
        };
        let mut response = HudResponse::default();
        let output = ctx.run_ui(input, |ui| {
            response = hud.show(ui, data);
        });
        output.drop_without_applying_deltas();
        response
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

    fn bag_slot_positions(profile: &Profile, count: usize) -> Vec<egui::Pos2> {
        let element = profile.get(ElementId::Bags);
        let rect = element.rect(screen(), frames::bags::size(count, &profile.style, element.scale));
        frames::bags::slot_rects(rect, count, &profile.style, element.scale)
            .map(|slot| slot.center())
            .collect()
    }

    /// **`foss-wow#79`, reproduced or refuted.** Every other click test in
    /// this file calls `hide_bars` first, which removes the frames that sit
    /// in the layer *above* the bag window -- so the whole suite has been
    /// asserting the one arrangement the player never has. This runs the
    /// identical click twice, bars hidden and bars visible, and the pair is
    /// the point: if only the second fails, a frame above the bags is eating
    /// the click and the report "mouse clicks through the bag window" is
    /// exactly right.
    #[test]
    fn a_bag_square_takes_a_click_with_the_action_bars_visible() {
        let slots = bag_slots(&[(2224, "Small Dagger", 1), (159, "Refreshing Spring Water", 5)]);
        let data = HudData {
            bags: Some(&slots),
            ..Default::default()
        };

        let quiet = {
            let mut hud = Hud::default();
            hide_bars(&mut hud);
            let centres = bag_slot_positions(&hud.profile, slots.len());
            drive(
                &mut hud,
                &data,
                &click_script(centres[0], egui::PointerButton::Primary),
            );
            hud.held
        };
        assert_eq!(
            quiet,
            Some(Held::Item(0)),
            "with the bars hidden, clicking a bag square must pick the item up"
        );

        let as_played = {
            let mut hud = Hud::default();
            let centres = bag_slot_positions(&hud.profile, slots.len());
            drive(
                &mut hud,
                &data,
                &click_script(centres[0], egui::PointerButton::Primary),
            );
            hud.held
        };
        assert_eq!(
            as_played, quiet,
            "the same click must do the same thing with the interface a player \
             actually has on screen -- if this differs, something above the \
             bag window is taking the click"
        );
    }

    /// The same click again, but with the bag window **opened into an
    /// interface that was already on screen** -- which is the only way it
    /// ever happens in play, and the arrangement that hid the questgiver
    /// stacking bug for a whole milestone.
    ///
    /// `drive` above builds a fresh context per script, so every area is new
    /// in one pass and egui leaves them in build order; that is the one
    /// arrangement where the draw loop's sequence decides the z-order. Here
    /// the bars, chat and unit frames exist for several passes first and the
    /// bags appear afterwards, so egui's own between-frame ordering is what
    /// answers.
    #[test]
    fn a_bag_square_takes_a_click_when_opened_over_a_live_interface() {
        let slots = bag_slots(&[(2224, "Small Dagger", 1), (159, "Refreshing Spring Water", 5)]);
        let mut hud = Hud::default();
        let centres = bag_slot_positions(&hud.profile, slots.len());
        let ctx = egui::Context::default();

        // The interface a player is looking at before they press B.
        let closed = HudData::default();
        for _ in 0..3 {
            pass(&ctx, &mut hud, &closed, Vec::new());
        }

        // Now the bags open, and the click follows.
        let open = HudData {
            bags: Some(&slots),
            ..Default::default()
        };
        for events in click_script(centres[0], egui::PointerButton::Primary) {
            pass(&ctx, &mut hud, &open, events);
        }

        assert_eq!(
            hud.held,
            Some(Held::Item(0)),
            "a bag square clicked in a window opened over the running interface \
             must pick the item up; picking nothing up is `foss-wow#79`"
        );
    }

    /// **The quantity the *viewer* branches on, which is not the one the two
    /// tests above assert.**
    ///
    /// Those check `hud.held` -- this crate's own view of its own click, and
    /// it is correct. But the window decides whether a press belongs to the
    /// interface *before* any of that, from `egui_state.on_window_event(..)
    /// .consumed`, which egui answers out of `wants_pointer_input`. If that
    /// comes back false the press never reaches this crate at all: the viewer
    /// treats it as a world click, grabs the cursor for a camera drag, and
    /// the bag window looks exactly like a window the mouse goes through.
    ///
    /// So this asserts the same property for a window that **works in play**
    /// (loot) and one that is **reported not to** (bags). Asserting only the
    /// broken one would pass the day someone made every window equally
    /// unclickable.
    #[test]
    fn egui_claims_the_pointer_over_the_bag_window_as_it_does_over_loot() {
        let rows = vec![
            frames::LootRow {
                take: frames::Take::Money,
                name: "2c".into(),
                count: 1,
                icon: None,
            },
            frames::LootRow {
                take: frames::Take::Item(7),
                name: "Frayed Shoes".into(),
                count: 1,
                icon: None,
            },
        ];
        let loot_data = HudData {
            loot: Some(&rows),
            ..Default::default()
        };
        let mut hud = Hud::default();
        let element = hud.profile.get(ElementId::Loot);
        let loot_at = element
            .rect(
                screen(),
                frames::loot::size(rows.len(), &hud.profile.style, element.scale),
            )
            .center();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            pass(&ctx, &mut hud, &loot_data, vec![egui::Event::PointerMoved(loot_at)]);
        }
        let over_loot = ctx.egui_wants_pointer_input();

        let slots = bag_slots(&[(2224, "Small Dagger", 1), (159, "Refreshing Spring Water", 5)]);
        let bag_data = HudData {
            bags: Some(&slots),
            ..Default::default()
        };
        let mut hud = Hud::default();
        let bag_at = bag_slot_positions(&hud.profile, slots.len())[0];
        let ctx = egui::Context::default();
        for _ in 0..3 {
            pass(&ctx, &mut hud, &bag_data, vec![egui::Event::PointerMoved(bag_at)]);
        }
        let over_bags = ctx.egui_wants_pointer_input();

        // The asymmetry that caused `foss-wow#79`, recorded rather than
        // asserted: egui counts the loot window and not the bag window,
        // because `ElementId::layer` puts one in `Order::Middle` and the
        // other in `Order::Background`. Not asserted, because it is egui's
        // behaviour rather than ours and an upgrade may reasonably change
        // it -- what must hold is the line below.
        assert!(
            over_loot,
            "the loot window takes clicks in play, so egui must want the pointer over it"
        );
        if !over_bags {
            eprintln!(
                "note: egui does not claim the pointer over a Background-order \
                 frame; `captures_pointer` is what covers the difference"
            );
        }

        // **The property the viewer must be able to rely on.** Its press
        // router asks this, not egui, precisely because of the asymmetry
        // above -- a press over *any* frame the interface drew has to read as
        // the interface's, or it starts a camera drag and grabs the cursor
        // out from under the click.
        assert!(
            hud.captures_pointer(&ctx),
            "`captures_pointer` must claim the bag window even where egui does \
             not -- this is what `foss-wow#79` turned on"
        );
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

    /// **Clicking a loot row reports what to ask the server for.**
    ///
    /// This is the check for a failure that is completely silent: a frame left
    /// out of the `Sense::click()` list draws correctly, hit-tests correctly,
    /// and never reports a click, so the arm handling it is dead code that
    /// looks alive. The loot window shipped that way and the symptom at the
    /// window was "it opens, and clicking does nothing".
    ///
    /// It also pins the thing that would be wrong in a much worse way: the
    /// row's *position* is not its loot slot. The second row here carries slot
    /// 7, and a click on it has to report 7.
    #[test]
    fn clicking_a_loot_row_reports_its_slot_not_its_position() {
        let rows = vec![
            frames::LootRow {
                take: frames::Take::Money,
                name: "2c".into(),
                count: 1,
                icon: None,
            },
            // Earlier slots already taken; the server still calls this 7.
            frames::LootRow {
                take: frames::Take::Item(7),
                name: "Frayed Shoes".into(),
                count: 1,
                icon: None,
            },
        ];
        let data = HudData {
            loot: Some(&rows),
            ..Default::default()
        };

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::Loot);
        let rect = element.rect(
            screen(),
            frames::loot::size(rows.len(), &hud.profile.style, element.scale),
        );
        let centres: Vec<egui::Pos2> =
            frames::loot::row_rects(rect, rows.len(), &hud.profile.style, element.scale)
                .map(|row| row.center())
                .collect();

        let response = drive(
            &mut hud,
            &data,
            &click_script(centres[1], egui::PointerButton::Primary),
        );
        assert_eq!(
            response.take_loot,
            Some(frames::Take::Item(7)),
            "a click on row 1 must ask for loot slot 7"
        );

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let response = drive(
            &mut hud,
            &data,
            &click_script(centres[0], egui::PointerButton::Primary),
        );
        assert_eq!(response.take_loot, Some(frames::Take::Money));
    }

    fn log_entries() -> Vec<frames::QuestLogEntry> {
        vec![
            frames::QuestLogEntry {
                id: 783,
                detail: frames::QuestDetail::Known {
                    title: "A Threat Within".into(),
                    objective: "Speak with Marshal McBride.".into(),
                    progress: Vec::new(),
                    level: 1,
                },
                complete: true,
            },
            frames::QuestLogEntry {
                id: 38,
                detail: frames::QuestDetail::Waiting,
                complete: false,
            },
        ]
    }

    /// **A quest in progress has to show the progress**, and it is the one
    /// line whose absence is invisible: a log that says `Kobold Camp Cleanup`
    /// and nothing else looks exactly like a log that is working.
    #[test]
    fn the_quest_log_counts_each_objective() {
        let entries = vec![frames::QuestLogEntry {
            id: 7,
            detail: frames::QuestDetail::Known {
                title: "Kobold Camp Cleanup".into(),
                objective: "Kill 8 Kobold Vermin.".into(),
                level: 2,
                progress: vec![
                    "Kobold Vermin: 4/8".into(),
                    "Large Candle: 0/3".into(),
                ],
            },
            complete: false,
        }];
        let data = HudData {
            quest_log: Some(&entries),
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let text = painted_text(&shapes(&mut hud, &data, None)).join(" | ");
        assert!(text.contains("Kobold Vermin: 4/8"), "{text}");
        assert!(text.contains("Large Candle: 0/3"), "{text}");
        // And the summary above them still draws -- the counted lines are an
        // addition, not a replacement.
        assert!(text.contains("Kill 8 Kobold Vermin."), "{text}");
    }

    /// The window has to grow for those lines, or the last quest in a log is
    /// drawn outside the frame and simply is not there.
    #[test]
    fn counted_objectives_make_the_log_taller() {
        let style = style::Style::default();
        let bare = vec![frames::QuestLogEntry {
            id: 7,
            detail: frames::QuestDetail::Known {
                title: "Kobold Camp Cleanup".into(),
                objective: "Kill 8 Kobold Vermin.".into(),
                level: 2,
                progress: Vec::new(),
            },
            complete: false,
        }];
        let mut counted = bare.clone();
        if let frames::QuestDetail::Known { progress, .. } = &mut counted[0].detail {
            progress.push("Kobold Vermin: 4/8".into());
        }
        assert!(
            frames::quest_log::size(&counted, &style, 1.0).y
                > frames::quest_log::size(&bare, &style, 1.0).y
        );
    }

    /// The quest log is drawn when it is open and not otherwise -- the same
    /// existence-is-the-flag rule the spellbook and bag window follow.
    #[test]
    fn a_quest_log_appears_only_when_open_or_editing() {
        let entries = log_entries();
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        assert!(
            painted(&mut hud, &HudData::default()).is_empty(),
            "a shut log must draw nothing"
        );

        let data = HudData {
            quest_log: Some(&entries),
            ..Default::default()
        };
        assert!(
            !painted(&mut hud, &data).is_empty(),
            "an open log must draw"
        );

        // And it can be positioned before the character has any quests.
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        hud.toggle_edit();
        assert!(!painted(&mut hud, &HudData::default()).is_empty());
    }

    /// A page with one pin and one region, for the two map tests below.
    fn map_view() -> frames::MapView {
        frames::MapView {
            title: "Elwynn Forest".into(),
            tiles: Default::default(),
            patches: Vec::new(),
            markers: vec![
                frames::MapMarker {
                    u: 0.48,
                    v: 0.44,
                    facing: 0.0,
                    kind: frames::MarkerKind::Objective,
                    label: "A Threat Within".into(),
                    outline: Vec::new(),
                },
                frames::MapMarker {
                    u: 0.3,
                    v: 0.3,
                    facing: 0.0,
                    kind: frames::MarkerKind::Player,
                    label: String::new(),
                    outline: Vec::new(),
                },
            ],
            note: None,
        }
    }

    /// **A region has to be drawn as a region.** A third of this realm's quest
    /// markers are polygons rather than points, and the difference between a
    /// ring and a pin is invisible in a rectangle-only check -- the same hole
    /// that let the loot window ship without ever receiving a click.
    #[test]
    fn a_quest_region_paints_more_than_a_pin_does() {
        fn shape_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
            fn walk(shape: &egui::Shape) -> usize {
                match shape {
                    egui::Shape::Vec(shapes) => shapes.iter().map(walk).sum(),
                    _ => 1,
                }
            }
            shapes.iter().map(|clipped| walk(&clipped.shape)).sum()
        }

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let pin = map_view();
        let plain = shape_count(&shapes(
            &mut hud,
            &HudData {
                world_map: Some(&pin),
                ..Default::default()
            },
            None,
        ));

        let mut region = map_view();
        region.markers[0].outline = vec![(0.4, 0.4), (0.4, 0.5), (0.5, 0.5), (0.5, 0.4)];
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let ringed = shape_count(&shapes(
            &mut hud,
            &HudData {
                world_map: Some(&region),
                ..Default::default()
            },
            None,
        ));
        assert!(
            ringed > plain,
            "a marker with an outline painted {ringed} shapes, the same pin without one {plain}"
        );
    }

    /// The pin is the only thing on the map that can say which quest it is
    /// for, so hovering it has to name one -- and hovering empty parchment
    /// must not, or every marker would look labelled.
    #[test]
    fn hovering_a_quest_pin_names_its_quest() {
        let view = map_view();
        let data = HudData {
            world_map: Some(&view),
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::WorldMap);
        let rect = element.rect(screen(), frames::world_map::size(&hud.profile.style, 1.0));
        let art = frames::world_map::art_rect(rect, &hud.profile.style, 1.0);

        let on_pin = frames::world_map::marker_pos(art, 0.48, 0.44);
        let text = painted_text(&shapes(&mut hud, &data, Some(on_pin))).join(" | ");
        assert!(text.contains("A Threat Within"), "{text}");

        // The same window, hovered where there is no marker: any difference is
        // the tooltip and nothing else.
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let empty = frames::world_map::marker_pos(art, 0.8, 0.8);
        let text = painted_text(&shapes(&mut hud, &data, Some(empty))).join(" | ");
        assert!(!text.contains("A Threat Within"), "{text}");
    }

    /// **The text a player would read, asserted from the painted shapes.**
    /// Every earlier frame here was checked only by its rectangle, and a
    /// rectangle is drawn identically whether the row inside it says the right
    /// thing or nothing at all.
    #[test]
    fn the_quest_log_paints_what_it_was_given() {
        let entries = log_entries();
        let data = HudData {
            quest_log: Some(&entries),
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let text = painted_text(&shapes(&mut hud, &data, None));
        let all = text.join(" | ");
        assert!(all.contains("A Threat Within"), "{all}");
        assert!(all.contains("Speak with Marshal McBride."), "{all}");
        // The finished quest says so, and the unfinished one does not -- the
        // half that would pass anyway if every row claimed to be complete.
        assert!(all.contains("(Complete)"), "{all}");
        // A quest still being asked about names its own id, so a player can
        // report a row that never resolves.
        assert!(all.contains("38"), "{all}");
    }

    /// **A quest waiting for its description must not look like one with no
    /// objectives.** This is the whole reason the cache has three states
    /// rather than an `Option`, and it is asserted where a player would see
    /// it: in the painted text.
    #[test]
    fn a_waiting_quest_does_not_paint_as_an_empty_one() {
        let waiting = vec![frames::QuestLogEntry {
            id: 38,
            detail: frames::QuestDetail::Waiting,
            complete: false,
        }];
        let empty = vec![frames::QuestLogEntry {
            id: 38,
            detail: frames::QuestDetail::Known {
                title: "Westfall Stew".into(),
                objective: String::new(),
                progress: Vec::new(),
                level: 13,
            },
            complete: false,
        }];

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let waiting_text = painted_text(&shapes(
            &mut hud,
            &HudData {
                quest_log: Some(&waiting),
                ..Default::default()
            },
            None,
        ))
        .join(" | ");
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let empty_text = painted_text(&shapes(
            &mut hud,
            &HudData {
                quest_log: Some(&empty),
                ..Default::default()
            },
            None,
        ))
        .join(" | ");
        assert_ne!(
            waiting_text, empty_text,
            "waiting and empty must be distinguishable on screen"
        );
        assert!(empty_text.contains("Westfall Stew"), "{empty_text}");
    }

    /// Clicking a row reports the **quest id**, not the row's position -- and
    /// the row clicked here is the second one, so a bug that always reported
    /// the first would fail.
    ///
    /// Same silent-failure shape as the loot window: a frame left out of the
    /// `Sense::click()` list draws and hit-tests fine and never reports a
    /// click, so the arm handling it is dead code that reads as live.
    #[test]
    fn clicking_a_quest_row_reports_its_id() {
        let entries = log_entries();
        let data = HudData {
            quest_log: Some(&entries),
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::QuestLog);
        let rect = element.rect(
            screen(),
            frames::quest_log::size(&entries, &hud.profile.style, element.scale),
        );
        let centres: Vec<egui::Pos2> =
            frames::quest_log::entry_rects(rect, &entries, &hud.profile.style, element.scale)
                .into_iter()
                .map(|row| row.center())
                .collect();

        let response = drive(
            &mut hud,
            &data,
            &click_script(centres[1], egui::PointerButton::Primary),
        );
        assert_eq!(
            response.selected_quest,
            Some(38),
            "clicking the second row must report quest 38, not row 1"
        );
    }

    /// **Accept and Close sit side by side in the questgiver window**, so a
    /// hit test that disagreed with the drawing would decline quests the
    /// player meant to take. Driven through the real event loop, which is also
    /// what proves the frame is in the `Sense::click()` list at all.
    /// **A window you have to answer must not be sealed under one you were
    /// only reading.** The map is the largest frame in the interface and sits
    /// dead centre; the questgiver window is anchored to the top edge and
    /// grows down into the same space. With both open, every frame is an egui
    /// area of the same order, so whichever is built last is on top -- and the
    /// Accept button was underneath, which reads as a window that has stopped
    /// working rather than as one that is behind something.
    #[test]
    fn the_accept_button_works_with_the_map_open_over_it() {
        // A real quest scroll, not a one-liner: Northshire's own run to
        // several paragraphs, and the window grows down into the map's space
        // exactly because the text is long.
        let view = frames::QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: (0..14)
                .map(|line| format!("Line {line} of what the questgiver says."))
                .collect::<Vec<_>>()
                .join("\n"),
            objectives: vec!["Speak with Marshal McBride.".into()],
            rewards: vec!["item 2224 x1".into()],
            action: frames::QuestgiverAction::Accept,
        };
        let map = map_view();
        let data = HudData {
            questgiver: Some(&view),
            world_map: Some(&map),
            ..Default::default()
        };

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::Questgiver);
        let rect = element.rect(
            screen(),
            frames::questgiver::size(&view, &hud.profile.style, element.scale),
        );
        let (accept, _) = frames::questgiver::button_rects(rect, &hud.profile.style, element.scale);
        // The premise of the test: the two windows really do overlap where the
        // button is, or this proves nothing about stacking.
        let map_element = hud.profile.get(ElementId::WorldMap);
        let map_rect = map_element.rect(
            screen(),
            frames::world_map::size(&hud.profile.style, map_element.scale),
        );
        assert!(
            map_rect.contains(accept.center()),
            "the map at {map_rect:?} does not cover the Accept button at {:?}",
            accept.center()
        );

        let response = drive(
            &mut hud,
            &data,
            &click_script(accept.center(), egui::PointerButton::Primary),
        );
        assert_eq!(response.questgiver.acted, Some(783));
    }

    /// **And the map opened *after* the questgiver window must not seal it
    /// either** -- which is the arrangement the live bug actually had, and the
    /// one the test above cannot produce.
    ///
    /// That test builds both windows into a fresh context, and that is the
    /// single arrangement in which egui orders same-layer areas by the
    /// sequence they were built: an area that was **not visible last frame**
    /// is moved to the top of its layer, so with both of them new the stable
    /// sort at the end of the pass leaves them exactly as the loop built them.
    /// Live they appear at different moments -- you greet an NPC, read the
    /// scroll, then press `M` -- and whichever appears second is moved above
    /// the other whatever order the loop builds them in. The draw-order loop
    /// is therefore not enough on its own, and the test that said it was
    /// passed because its premise had quietly stopped holding.
    #[test]
    fn a_map_opened_after_the_questgiver_does_not_seal_it() {
        let view = frames::QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: (0..14)
                .map(|line| format!("Line {line} of what the questgiver says."))
                .collect::<Vec<_>>()
                .join("\n"),
            objectives: vec!["Speak with Marshal McBride.".into()],
            rewards: vec!["item 2224 x1".into()],
            action: frames::QuestgiverAction::Accept,
        };
        let map = map_view();

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::Questgiver);
        let rect = element.rect(
            screen(),
            frames::questgiver::size(&view, &hud.profile.style, element.scale),
        );
        let (accept, _) = frames::questgiver::button_rects(rect, &hud.profile.style, element.scale);
        let map_element = hud.profile.get(ElementId::WorldMap);
        let map_rect = map_element.rect(
            screen(),
            frames::world_map::size(&hud.profile.style, map_element.scale),
        );
        // The premise, asserted before anything else: the two really do
        // overlap where the button is.
        assert!(
            map_rect.contains(accept.center()),
            "the map at {map_rect:?} does not cover the Accept button at {:?}",
            accept.center()
        );

        let ctx = egui::Context::default();
        let greeted = HudData {
            questgiver: Some(&view),
            ..Default::default()
        };
        let and_the_map = HudData {
            questgiver: Some(&view),
            world_map: Some(&map),
            ..Default::default()
        };
        // The scroll is open and read for a couple of passes...
        for _ in 0..2 {
            pass(&ctx, &mut hud, &greeted, Vec::new());
        }
        // ...and only then is the map opened over it.
        let mut response = HudResponse::default();
        for events in click_script(accept.center(), egui::PointerButton::Primary) {
            response = pass(&ctx, &mut hud, &and_the_map, events);
        }
        assert_eq!(
            response.questgiver.acted,
            Some(783),
            "the map was opened second and swallowed the Accept button"
        );
    }

    /// **The same thing for a panel, which is the rank the pin is for.** The
    /// questgiver window is in a different egui layer from the map and could
    /// not be sealed by it whatever else happened; the quest log is in the
    /// *same* layer, one rank above, and stays there only because it asks to
    /// be on top of that layer every frame. Reading the log and then opening
    /// the map over it is the obvious thing to do with the two of them.
    #[test]
    fn a_map_opened_after_the_quest_log_does_not_seal_it() {
        let entries = log_entries();
        let map = map_view();

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        // **Moved onto the map on purpose.** The default layout keeps the log
        // off to one side, so with the shipped positions these two never
        // overlap and the test would prove nothing -- but every position here
        // belongs to the user, and a log dragged over the middle of the screen
        // is an ordinary thing to have done. The rank has to hold for the
        // layout somebody made, not only for the one that shipped.
        {
            let log = hud.profile.edit(ElementId::QuestLog);
            log.anchor = crate::element::Anchor::Center;
            log.offset = [0.0, 0.0];
        }
        let element = hud.profile.get(ElementId::QuestLog);
        let rect = element.rect(
            screen(),
            frames::quest_log::size(&entries, &hud.profile.style, element.scale),
        );
        let row = frames::quest_log::entry_rects(rect, &entries, &hud.profile.style, element.scale)
            [1]
        .center();
        let map_element = hud.profile.get(ElementId::WorldMap);
        let map_rect = map_element.rect(
            screen(),
            frames::world_map::size(&hud.profile.style, map_element.scale),
        );
        // The premise again: with no overlap this proves nothing at all.
        assert!(
            map_rect.contains(row),
            "the map at {map_rect:?} does not cover the quest row at {row:?}"
        );

        let ctx = egui::Context::default();
        let log_only = HudData {
            quest_log: Some(&entries),
            ..Default::default()
        };
        let and_the_map = HudData {
            quest_log: Some(&entries),
            world_map: Some(&map),
            ..Default::default()
        };
        for _ in 0..2 {
            pass(&ctx, &mut hud, &log_only, Vec::new());
        }
        let mut response = HudResponse::default();
        for events in click_script(row, egui::PointerButton::Primary) {
            response = pass(&ctx, &mut hud, &and_the_map, events);
        }
        assert_eq!(
            response.selected_quest,
            Some(38),
            "the map was opened over the quest log and swallowed the row"
        );
    }

    /// Accept and Close sit side by side, so a hit test that disagreed with
    /// the drawing would decline quests the player meant to take.
    #[test]
    fn pressing_accept_reports_the_quest_and_pressing_close_does_not() {
        let view = frames::QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: "Speak with Marshal McBride.".into(),
            objectives: Vec::new(),
            rewards: Vec::new(),
            action: frames::QuestgiverAction::Accept,
        };
        let data = HudData {
            questgiver: Some(&view),
            ..Default::default()
        };

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::Questgiver);
        let rect = element.rect(
            screen(),
            frames::questgiver::size(&view, &hud.profile.style, element.scale),
        );
        let (accept, close) =
            frames::questgiver::button_rects(rect, &hud.profile.style, element.scale);

        let response = drive(
            &mut hud,
            &data,
            &click_script(accept.center(), egui::PointerButton::Primary),
        );
        assert_eq!(response.questgiver.acted, Some(783));
        assert!(!response.questgiver.closed, "Accept must not also close");

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let response = drive(
            &mut hud,
            &data,
            &click_script(close.center(), egui::PointerButton::Primary),
        );
        assert!(response.questgiver.closed);
        assert_eq!(response.questgiver.acted, None, "Close must not accept");
    }

    /// The window draws the quest's own text, not a placeholder -- a rectangle
    /// is painted identically whether the words inside it are right or absent.
    #[test]
    fn the_questgiver_window_paints_the_quest_it_was_given() {
        let view = frames::QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: "Speak with Marshal McBride.".into(),
            objectives: Vec::new(),
            rewards: Vec::new(),
            action: frames::QuestgiverAction::Accept,
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let text = painted_text(&shapes(
            &mut hud,
            &HudData {
                questgiver: Some(&view),
                ..Default::default()
            },
            None,
        ))
        .join(" | ");
        assert!(text.contains("A Threat Within"), "{text}");
        assert!(text.contains("Marshal McBride"), "{text}");
        assert!(text.contains("Accept"), "{text}");
    }

    /// The same silent-failure shape as the loot-row test above, for the
    /// release prompt: a frame missing from the `Sense::click()` list draws
    /// and hit-tests fine and simply never reports a click.
    #[test]
    fn clicking_the_release_prompt_reports_it() {
        let view = frames::ReleasePromptView {
            text: "You have died.".into(),
        };
        let data = HudData {
            release_prompt: Some(&view),
            ..Default::default()
        };

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::ReleasePrompt);
        let rect = element.rect(screen(), frames::release::size(&hud.profile.style, element.scale));

        let response = drive(&mut hud, &data, &click_script(rect.center(), egui::PointerButton::Primary));
        assert!(response.release_clicked, "a click on the prompt was not reported");
    }

    /// And the same asymmetry as the cast bar and the loot window: absent
    /// while alive, present once there is something to release.
    #[test]
    fn a_release_prompt_appears_only_while_dead_or_editing() {
        let mut quiet = Hud::default();
        hide_bars(&mut quiet);
        assert!(
            painted(&mut quiet, &HudData::default()).is_empty(),
            "a release prompt was painted while alive"
        );

        let view = frames::ReleasePromptView {
            text: "You have died.".into(),
        };
        let mut open = Hud::default();
        hide_bars(&mut open);
        assert!(
            !painted(
                &mut open,
                &HudData {
                    release_prompt: Some(&view),
                    ..Default::default()
                }
            )
            .is_empty(),
            "a release prompt painted nothing while dead"
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

    fn bag_slots(items: &[(u32, &str, u32)]) -> Vec<frames::BagSlot> {
        let mut slots = vec![frames::BagSlot::default(); 16];
        for (index, (entry, name, count)) in items.iter().enumerate() {
            slots[index] = frames::BagSlot {
                item: Some(frames::BagItem {
                    entry: *entry,
                    name: name.to_string(),
                    count: *count,
                    icon: None,
                }),
            };
        }
        slots
    }

    /// The same gesture as the spellbook's, on `self.held` rather than a
    /// second mechanism -- and the same row-is-not-a-slot caveat as loot:
    /// this reports *positions in the list it was handed*, never a real
    /// `(bag, slot)` pair, because this crate has no idea what those are.
    #[test]
    fn clicking_two_bag_slots_reports_a_move() {
        let slots = bag_slots(&[(2589, "Linen Cloth", 3), (159, "Refreshing Spring Water", 5)]);
        let data = HudData {
            bags: Some(&slots),
            ..Default::default()
        };
        let profile = Profile::default();
        let positions = bag_slot_positions(&profile, slots.len());

        let mut hud = Hud::default();
        let mut script = click_script(positions[0], egui::PointerButton::Primary);
        script.extend(click_script(positions[5], egui::PointerButton::Primary));
        let response = drive(&mut hud, &data, &script);

        assert_eq!(
            response.move_item,
            Some((0, 5)),
            "picking up row 0 and clicking row 5 must report exactly that move"
        );
        assert_eq!(hud.held, None, "the hold must end once the move is reported");
    }

    /// Clicking the same slot twice puts the item back where it was, rather
    /// than reporting a move onto itself -- which the caller would have no
    /// sensible way to act on.
    #[test]
    fn clicking_the_same_bag_slot_twice_cancels_the_hold() {
        let slots = bag_slots(&[(2589, "Linen Cloth", 3)]);
        let data = HudData {
            bags: Some(&slots),
            ..Default::default()
        };
        let profile = Profile::default();
        let positions = bag_slot_positions(&profile, slots.len());

        let mut hud = Hud::default();
        let mut script = click_script(positions[0], egui::PointerButton::Primary);
        script.extend(click_script(positions[0], egui::PointerButton::Primary));
        let response = drive(&mut hud, &data, &script);

        assert_eq!(response.move_item, None);
        assert_eq!(hud.held, None);
    }

    /// A right-click with nothing held asks to auto-equip, and does not also
    /// pick the item up -- the two gestures answer different questions and a
    /// right-click starting a hold would leave the next left-click meaning
    /// something the user never asked for.
    #[test]
    fn right_clicking_a_bag_slot_reports_the_gesture() {
        let slots = bag_slots(&[(2589, "Linen Cloth", 3)]);
        let data = HudData {
            bags: Some(&slots),
            ..Default::default()
        };
        let profile = Profile::default();
        let positions = bag_slot_positions(&profile, slots.len());

        let mut hud = Hud::default();
        let response = drive(
            &mut hud,
            &data,
            &click_script(positions[0], egui::PointerButton::Secondary),
        );

        assert_eq!(response.activate_item, Some(0));
        assert_eq!(hud.held, None, "a right-click must not also start a hold");
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
        assert_eq!(
            hud.held,
            Some(Held::Spell(entries[0].id)),
            "the click picked nothing up"
        );

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
            ElementId::QuestLog => {
                frames::quest_log::size(&frames::quest_log::placeholder(), &profile.style, scale)
            }
            ElementId::Questgiver => {
                frames::questgiver::size(&frames::questgiver::placeholder(), &profile.style, scale)
            }
            ElementId::WorldMap => frames::world_map::size(&profile.style, scale),
            ElementId::Loot => {
                frames::loot::size(frames::loot::placeholder().len(), &profile.style, scale)
            }
            ElementId::ReleasePrompt => frames::release::size(&profile.style, scale),
            ElementId::PartyFrame => frames::party::size(
                &frames::PartyMemberView::placeholder(),
                &profile.style,
                scale,
            ),
            ElementId::PartyInvite => frames::party_invite::size(&profile.style, scale),
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
        //
        // **The world map is excluded here and asserted separately below.**
        // It is 760 by 520 in the middle of the screen and there is nowhere on
        // any screen to put a window that size without touching something; a
        // map is the one frame whose job is to cover the view while it is
        // open. What has to be true of it is narrower and is a real claim
        // rather than an exemption -- see the test that follows.
        let shown: Vec<(ElementId, egui::Rect)> = ElementId::ALL
            .into_iter()
            .filter(|id| profile.get(*id).visible && *id != ElementId::WorldMap)
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

    /// The map may cover a window somebody opened; it may not cover the frames
    /// that are simply *there*.
    ///
    /// The distinction is the whole reason the map is left out of the test
    /// above. A player who opens the map has chosen to stop and read it, and
    /// the loot window or the release prompt underneath it can be dealt with
    /// by closing the map -- but health, target, chat and the action bars are
    /// not things anyone opened, and a map that hid them would be taking the
    /// game away rather than putting something on top of it.
    #[test]
    fn the_world_map_covers_only_windows_that_were_opened() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let profile = Profile::default();
        let map = profile
            .get(ElementId::WorldMap)
            .rect(screen, size_of(&profile, ElementId::WorldMap));
        // Everything that appears without the player asking for it.
        for id in [
            ElementId::PlayerFrame,
            ElementId::TargetFrame,
            ElementId::ChatFrame,
            ElementId::CastBar,
            ElementId::ActionBar1,
            ElementId::ActionBar2,
            ElementId::ActionBar3,
        ] {
            let rect = profile.get(id).rect(screen, size_of(&profile, id));
            assert!(
                !map.intersects(rect),
                "the world map at {map:?} covers {} at {rect:?}",
                id.label()
            );
        }
        // The party frame is in that list on purpose: it appears because
        // somebody grouped up, not because a key was pressed, and a map
        // covering it would hide who is dying.
        let party = profile
            .get(ElementId::PartyFrame)
            .rect(screen, size_of(&profile, ElementId::PartyFrame));
        assert!(
            !map.intersects(party),
            "the world map at {map:?} covers the party frame at {party:?}"
        );
    }

    fn party() -> Vec<frames::PartyMemberView> {
        frames::PartyMemberView::placeholder()
    }

    /// The party frame appears with a group and not without one -- emptiness
    /// is the flag, the same rule the loot window follows. Asserting only that
    /// it appears with a group would pass just as well if it appeared always,
    /// which would put an empty box on the screen of every solo player.
    #[test]
    fn a_party_frame_appears_only_with_a_group_or_while_editing() {
        let members = party();
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let with = painted_text(&shapes(
            &mut hud,
            &HudData {
                party: &members,
                ..Default::default()
            },
            None,
        ))
        .join(" | ");
        assert!(with.contains("Watcher"), "{with}");

        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let without = painted_text(&shapes(&mut hud, &HudData::default(), None)).join(" | ");
        assert!(
            !without.contains("Watcher"),
            "a party frame drew with nobody in the group: {without}"
        );

        // ...and in edit mode regardless, or it could only be positioned
        // while two other people were logged in and stayed grouped for the
        // length of the drag.
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        hud.edit.active = true;
        let editing = painted_text(&shapes(&mut hud, &HudData::default(), None)).join(" | ");
        assert!(
            editing.contains("Watcher"),
            "the party frame could not be positioned without a group: {editing}"
        );
    }

    /// Clicking a member reports their **guid**, not the row -- and the row
    /// clicked is the *second*, so a bug that always reported the first would
    /// fail. Driven through the real event loop, which is also what proves the
    /// frame is in the `Sense::click()` list: one left out of it draws
    /// correctly, hit-tests correctly, and never reports a thing.
    #[test]
    fn clicking_a_party_member_reports_their_guid() {
        let members = party();
        let data = HudData {
            party: &members,
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::PartyFrame);
        let style = hud.profile.style;
        let rect = element.rect(
            screen(),
            frames::party::size(&members, &style, element.scale),
        );
        // The second row's centre, measured the way the frame stacks rows
        // rather than by dividing the height: the rows are not all the same
        // height, so an averaged position would test a pixel no player's
        // click would land on.
        let row_height = |member: &frames::PartyMemberView| {
            frames::party::size(std::slice::from_ref(member), &style, element.scale).y
                - style.padding * 2.0 * element.scale
        };
        let top = rect.top()
            + style.padding * element.scale
            + row_height(&members[0])
            + style.gap * element.scale;
        let point = egui::Pos2::new(rect.center().x, top + row_height(&members[1]) * 0.5);

        let response = drive(
            &mut hud,
            &data,
            &click_script(point, egui::PointerButton::Primary),
        );
        assert_eq!(
            response.party_target,
            Some(members[1].guid),
            "clicking the second row must report that member, not the first"
        );
    }

    /// **Accept and Decline are opposite answers**, so a hit test that
    /// disagreed with the drawing would put the character in a group they
    /// refused. Both are asserted, plus a press between them: a rule that
    /// answered `Accept` for every pixel would pass a test of Accept alone.
    #[test]
    fn the_invite_buttons_answer_separately() {
        let invite = frames::PartyInviteView::placeholder();
        let data = HudData {
            party_invite: Some(&invite),
            ..Default::default()
        };
        let mut hud = Hud::default();
        hide_bars(&mut hud);
        let element = hud.profile.get(ElementId::PartyInvite);
        let style = hud.profile.style;
        let scale = element.scale;
        let rect = element.rect(screen(), frames::party_invite::size(&style, scale));
        // The two button centres, derived from the frame the way the drawing
        // does rather than guessed at: the left and right halves of the strip
        // along the bottom.
        let button_y =
            rect.bottom() - style.padding * scale - style.party_invite_button_height * scale * 0.5;
        let quarter = rect.width() * 0.25;
        let accept = egui::Pos2::new(rect.left() + quarter, button_y);
        let decline = egui::Pos2::new(rect.right() - quarter, button_y);

        let mut accepting = Hud::default();
        hide_bars(&mut accepting);
        assert_eq!(
            drive(
                &mut accepting,
                &data,
                &click_script(accept, egui::PointerButton::Primary)
            )
            .party_invite,
            Some(frames::InviteAnswer::Accept)
        );

        let mut declining = Hud::default();
        hide_bars(&mut declining);
        assert_eq!(
            drive(
                &mut declining,
                &data,
                &click_script(decline, egui::PointerButton::Primary)
            )
            .party_invite,
            Some(frames::InviteAnswer::Decline)
        );

        // The text between them answers nothing. An accidental accept has to
        // be undone by leaving the group; an ignored press costs nothing.
        let mut missing = Hud::default();
        hide_bars(&mut missing);
        let text = egui::Pos2::new(rect.center().x, rect.top() + style.padding * scale + 2.0);
        assert_eq!(
            drive(
                &mut missing,
                &data,
                &click_script(text, egui::PointerButton::Primary)
            )
            .party_invite,
            None,
            "a press on the prompt's text answered the invite"
        );
    }

    /// **An invite must not be sealed under the map.** Exactly the failure
    /// `e9d001c` fixed for the questgiver's Accept button, and an invite is
    /// the worse case: it times out, so a prompt that cannot be reached is a
    /// group the player never joins with no error anywhere. The two frames are
    /// opened at *different moments* against one context, because egui's
    /// z-order persists between frames and a fresh context hides the bug
    /// entirely -- which is how the first attempt at the questgiver fix passed
    /// its own test while changing nothing.
    #[test]
    fn the_invite_buttons_work_with_the_map_open_over_them() {
        let invite = frames::PartyInviteView::placeholder();
        let map = map_view();
        let mut hud = Hud::default();
        hide_bars(&mut hud);

        let element = hud.profile.get(ElementId::PartyInvite);
        let style = hud.profile.style;
        let scale = element.scale;
        let rect = element.rect(screen(), frames::party_invite::size(&style, scale));
        let map_element = hud.profile.get(ElementId::WorldMap);
        let map_rect = map_element.rect(
            screen(),
            frames::world_map::size(&style, map_element.scale),
        );
        // The premise, asserted rather than assumed: with no overlap this
        // test proves nothing and would keep passing after a regression.
        assert!(
            map_rect.intersects(rect),
            "the map at {map_rect:?} does not reach the invite prompt at {rect:?}"
        );

        let accept = egui::Pos2::new(
            rect.left() + rect.width() * 0.25,
            rect.bottom() - style.padding * scale - style.party_invite_button_height * scale * 0.5,
        );

        let ctx = egui::Context::default();
        let mut last = HudResponse::default();
        // The invite first, then the map opened over it -- the order that
        // moves the map to the top of its layer, which is what broke the
        // questgiver window.
        let with_invite = HudData {
            party_invite: Some(&invite),
            ..Default::default()
        };
        let with_both = HudData {
            party_invite: Some(&invite),
            world_map: Some(&map),
            ..Default::default()
        };
        pass(&ctx, &mut hud, &with_invite, Vec::new());
        pass(&ctx, &mut hud, &with_both, Vec::new());
        for events in click_script(accept, egui::PointerButton::Primary) {
            last = pass(&ctx, &mut hud, &with_both, events);
        }
        assert_eq!(
            last.party_invite,
            Some(frames::InviteAnswer::Accept),
            "the map swallowed the invite's Accept button"
        );
    }
}
