//! Between the replicated world and the interface.
//!
//! Two directions. Outward, replicated fields become the plain [`ui::UnitView`]
//! snapshots the interface draws -- the `ui` crate deliberately knows nothing
//! about `world`, so something has to do the translation, and doing it here
//! keeps a rendering crate free of protocol types and vice versa.
//!
//! Inward, a click becomes a target: a ray through the cursor, tested against
//! each replicated entity's model bounds, nearest hit wins.

use glam::Vec3;
use render::camera::Ray;

use crate::live;

/// A click target is never smaller than this, whatever the model says.
///
/// Some models report a bounding box that is tiny or empty. Left alone, those
/// creatures would be on screen and unclickable, which reads as targeting
/// being broken rather than as one model's metadata being thin.
const MIN_RADIUS: f32 = 0.5;
const MIN_HEIGHT: f32 = 1.0;

/// Turns a replicated unit into what the interface draws.
pub fn unit_view(entity: &::world::state::Entity, name: String) -> ui::UnitView {
    ui::UnitView {
        name,
        level: entity.level(),
        // A field that has not arrived yet is zero rather than absent: an
        // empty bar is the honest picture of "we do not know", and it fills in
        // by itself on the next update.
        health: entity.health().unwrap_or(0),
        max_health: entity.max_health().unwrap_or(0),
        power: entity.power().unwrap_or(0),
        max_power: entity.max_power().unwrap_or(0),
        power_type: entity
            .power_type()
            .map(ui::PowerType::from_id)
            .unwrap_or_default(),
        // `false` for anything that is not a player: `is_ghost` reads a
        // field nothing else ever sets.
        ghost: entity.is_ghost(),
        // Filled in by the caller, which is the only place that knows
        // whether this snapshot is *the* target frame -- see
        // `ui::UnitView::combo_points`'s own doc comment.
        combo_points: None,
    }
}

/// A target frame for a party member who is not currently a replicated
/// object -- the case this whole milestone exists for, and the one
/// [`unit_view`] cannot serve because it reads fields off an `Entity` that
/// does not exist here.
///
/// Built from [`::world::state::WorldState::party_member_vitals`], which is
/// exactly what the party frame itself reads. The power bar is the one field
/// that does not simply carry an `Option`'s absence through as zero: **a
/// known `max_power` with an unknown `power_type` is drawn as no bar at all,
/// not a mana-coloured guess.** `power_type` is an index, and the party
/// packet can report a changed maximum before it has ever reported the type
/// -- see [`::world::state::PartyVitals::power_type`]'s own doc comment,
/// which is the field this project already found could be silently wrong.
///
/// `None` when the guid does not name a current party member, so a stale
/// target from a group that has since disbanded does not linger as a frame
/// with nothing behind it.
pub fn party_target_view(state: &::world::WorldState, guid: u64) -> Option<ui::UnitView> {
    let party = state.party.as_ref()?;
    let member = party.member(guid)?;
    let vitals = state.party_member_vitals(guid);
    let (power, max_power, power_type) =
        match (vitals.power, vitals.max_power, vitals.power_type) {
            (Some(power), Some(max_power), Some(power_type)) => {
                (power, max_power, ui::PowerType::from_id(power_type))
            }
            _ => (0, 0, ui::PowerType::default()),
        };
    Some(ui::UnitView {
        name: member.name.clone(),
        level: vitals.level,
        health: vitals.health.unwrap_or(0),
        max_health: vitals.max_health.unwrap_or(0),
        power,
        max_power,
        power_type,
        ghost: member.status & ::world::group::MemberStatus::GHOST != 0,
        // A party member out of view cannot be the combo target: combo
        // points are private to the owner and only ever drawn against a
        // replicated unit, which this frame by definition is not.
        combo_points: None,
    })
}

/// What to call a unit.
///
/// 3.3.5a puts no name in an object update: a player's comes from
/// `SMSG_NAME_QUERY_RESPONSE` and a creature's from
/// `SMSG_CREATURE_QUERY_RESPONSE`. Both are in the cache by the time a frame
/// draws, usually -- but "usually" is the point of the fallback. A name that
/// has been asked for and not yet answered has to show *something*, and
/// showing what is actually known beats a blank that looks like a bug.
pub fn unit_name(state: &::world::WorldState, entity: &::world::state::Entity) -> String {
    if entity.is_player() {
        if let Some(Some(name)) = state.names.player(entity.guid) {
            return name.to_string();
        }
        return format!("Player {:x}", entity.guid & 0xFFFF);
    }
    let entry = entity.fields.get(::world::update::fields::OBJECT_ENTRY);
    if let Some(Some(name)) = entry.and_then(|entry| state.names.creature(entry)) {
        return name.to_string();
    }
    match entry {
        Some(entry) => format!("Creature {entry}"),
        None => entity.object_type.name().to_string(),
    }
}

/// Turns a line off the wire into a line for the scrollback.
///
/// The speaker has a three-way fallback for a reason: a creature names itself
/// in the packet, a player does not and has to come from the cache, and a
/// player who is out of visibility range was never in replicated state to be
/// asked about. That last case is not hypothetical -- a whisper arrives from
/// someone who may be on the other side of the continent, and it is exactly
/// the case that shows up as a bare guid if nobody asks.
pub fn chat_entry(message: &::world::ChatMessage, state: &::world::WorldState) -> ui::ChatEntry {
    use ::world::ChatType;

    let who = if let Some(name) = &message.sender_name {
        Some(name.clone())
    } else if let Some(Some(name)) = state.names.player(message.sender) {
        Some(name.to_string())
    } else if message.sender == 0 {
        None
    } else {
        Some(format!("{:x}", message.sender & 0xFFFF))
    };

    let prefix = match message.chat_type {
        ChatType::Channel => message.channel.clone(),
        ChatType::Yell => Some("yell".into()),
        ChatType::Whisper => Some("whisper".into()),
        ChatType::WhisperInform => Some("to".into()),
        ChatType::Party => Some("party".into()),
        // Guild, and only guild. `Officer` is a separate wire type that
        // nothing here has ever sent or received, so it keeps falling
        // through to no tag rather than borrowing this one -- an officer
        // line labelled `[guild]` would be a *wrong* claim about who heard
        // it, which is worse than an unlabelled one.
        ChatType::Guild => Some("guild".into()),
        _ => None,
    };

    ui::ChatEntry {
        kind: chat_kind(message.chat_type),
        who,
        text: message.text.clone(),
        prefix,
    }
}

/// One swing as a line of scrollback.
///
/// The sentence itself is built in `world::combat`, so the viewer and
/// `wow-cli` cannot drift into describing the same fight differently. What
/// belongs here is only the naming: the same resolution chat lines use, so a
/// creature reads as `Kobold Vermin` rather than as a guid, and reads that way
/// retroactively once its name arrives.
pub fn combat_entry(
    swing: &::world::combat::MeleeSwing,
    own: u64,
    state: &::world::WorldState,
) -> ui::ChatEntry {
    ui::ChatEntry {
        kind: ui::ChatKind::Combat,
        // No speaker: the sentence already names both parties, and a "who"
        // column in front of "You hit X for 4" would say it twice.
        who: None,
        text: ::world::combat::describe_swing(swing, own, |guid| match state.get(guid) {
            Some(entity) => unit_name(state, entity),
            None => format!("{guid:#x}"),
        }),
        prefix: None,
    }
}

/// The same, for a spell that landed.
///
/// Its own function rather than a branch inside [`combat_entry`] because the
/// two take different packets -- but they produce the same *kind* of line on
/// purpose: a reader should learn what happened, not which opcode carried it.
///
/// The spell is named from `Spell.dbc` when the spellbook has been read, and
/// falls back to `spell 5176` when it has not. A number nobody can check is
/// worse than a blank, and an id is at least checkable.
pub fn spell_combat_entry(
    hit: &::world::combat::SpellDamage,
    own: u64,
    state: &::world::WorldState,
    spells: Option<&crate::spells::Spellbook>,
) -> ui::ChatEntry {
    ui::ChatEntry {
        kind: ui::ChatKind::Combat,
        who: None,
        text: ::world::combat::describe_spell_damage(
            hit,
            own,
            |guid| match state.get(guid) {
                Some(entity) => unit_name(state, entity),
                None => format!("{guid:#x}"),
            },
            |id| spells.and_then(|book| book.known_name(id)),
        ),
        prefix: None,
    }
}

/// The protocol's many chat types collapsed into the handful that read
/// differently to a person.
fn chat_kind(chat_type: ::world::ChatType) -> ui::ChatKind {
    use ::world::ChatType;
    match chat_type {
        ChatType::Say | ChatType::MonsterSay => ui::ChatKind::Say,
        ChatType::Yell | ChatType::MonsterYell => ui::ChatKind::Yell,
        ChatType::Whisper
        | ChatType::WhisperInform
        | ChatType::WhisperForeign
        | ChatType::MonsterWhisper
        | ChatType::RaidBossWhisper => ui::ChatKind::Whisper,
        ChatType::Emote
        | ChatType::TextEmote
        | ChatType::MonsterEmote
        | ChatType::RaidBossEmote => ui::ChatKind::Emote,
        ChatType::System => ui::ChatKind::System,
        ChatType::Channel => ui::ChatKind::Channel,
        ChatType::Party => ui::ChatKind::Party,
        ChatType::Guild => ui::ChatKind::Guild,
        _ => ui::ChatKind::Other,
    }
}

/// Guids and creature entries whose names are worth asking for.
///
/// Returned rather than queried here because asking needs the connection and
/// scanning needs the state, and holding both at once is what the borrow
/// checker exists to prevent. `limit` bounds how many go out per frame: the
/// name cache already stops a guid being asked twice, but a fresh login has a
/// hundred things in range at once and there is no reason to send a hundred
/// packets in one frame to learn them.
pub fn names_to_ask(
    state: &mut ::world::WorldState,
    extra_players: &[u64],
    limit: usize,
) -> Vec<NameRequest> {
    let now = std::time::Instant::now();
    let mut wanted: Vec<(bool, u64, u32)> = Vec::new();
    for entity in state.iter() {
        let entry = entity
            .fields
            .get(::world::update::fields::OBJECT_ENTRY)
            .unwrap_or(0);
        wanted.push((entity.is_player(), entity.guid, entry));
    }
    // Game objects, asked about separately because the question is different:
    // the others are asked what they are *called* and these are asked what
    // they *are*. A starting zone holds forty of them and the answer is
    // cached per entry like a creature's, so this costs one query per kind
    // and never repeats.
    let objects: Vec<(u64, u32)> = state
        .game_objects()
        .filter_map(|object| object.entry().map(|entry| (object.guid, entry)))
        .collect();
    // Chat senders who are not in range, and so were never in the sweep above.
    for guid in extra_players {
        wanted.push((true, *guid, 0));
    }

    let mut asking = Vec::new();
    for (is_player, guid, entry) in wanted {
        if asking.len() >= limit {
            break;
        }
        if is_player {
            if state.names.claim_player(guid, now) {
                asking.push(NameRequest::Player { guid });
            }
        } else if entry != 0 && state.names.claim_creature(entry, now) {
            asking.push(NameRequest::Creature { entry, guid });
        }
    }
    for (guid, entry) in objects {
        if asking.len() >= limit {
            break;
        }
        if entry != 0 && state.names.claim_gameobject(entry, now) {
            asking.push(NameRequest::GameObject { entry, guid });
        }
    }
    asking
}


/// Turns the replicated group into the rows the party frame draws.
///
/// **Two sources per member, and which one answers is not the caller's
/// choice.** `world::WorldState::party_member_vitals` prefers the replicated
/// entity where there is one and falls back to what
/// `SMSG_PARTY_MEMBER_STATS` last said where there is not -- so a member
/// standing next to you shows exact, current numbers and a member two zones
/// away shows a remembered summary rather than a blank. Every one of those
/// numbers is an `Option` all the way to the screen, because a member who has
/// never been in view and whose stats packet has not arrived is a real and
/// common state: a name and a guid and nothing else.
///
/// The **name** does not come from the name cache. A group list carries every
/// member's name in the packet itself, which is the one thing the party
/// protocol knows that the entity table cannot -- an unreplicated player is
/// not in the cache at all, and asking for their name by guid would leave the
/// frame reading `Player 3` until the reply arrived.
pub fn party_view(state: &::world::WorldState) -> Vec<ui::PartyMemberView> {
    let Some(party) = state.party.as_ref() else {
        return Vec::new();
    };
    party
        .members
        .iter()
        .map(|member| {
            let vitals = state.party_member_vitals(member.guid);
            ui::PartyMemberView {
                name: member.name.clone(),
                guid: member.guid,
                level: vitals.level,
                health: vitals.health,
                max_health: vitals.max_health,
                power: vitals.power,
                max_power: vitals.max_power,
                // Mapped only where one arrived. `PowerType::from_id` turns
                // every number into a variant, so a `unwrap_or_default` here
                // would silently promote "not known" to mana and paint a
                // rogue's frame blue with nothing ever saying it was a guess.
                power_type: vitals.power_type.map(ui::PowerType::from_id),
                online: member.is_online(),
                dead: member.is_dead(),
                leader: party.is_leader(member.guid),
            }
        })
        .collect()
}

/// The party's current loot rule, or `None` when the group has nobody else
/// in it -- see `world::group::Party::loot`'s doc comment for why that is a
/// real state and not a bug to work around. `editable` is set only when
/// `own_guid` leads the group: the server refuses `CMSG_LOOT_METHOD` from
/// anyone else in silence, and this client refuses it locally first rather
/// than sending a request it already knows will be dropped.
pub fn party_loot_view(state: &::world::WorldState, own_guid: u64) -> Option<ui::LootRuleView> {
    let party = state.party.as_ref()?;
    let rule = party.loot.as_ref()?;
    let master_name = (rule.master != 0)
        .then(|| state.names.player(rule.master))
        .flatten()
        .flatten()
        .map(str::to_string);
    Some(ui::LootRuleView {
        label: ::world::group::describe_loot_rule(rule, master_name.as_deref()),
        editable: party.is_leader(own_guid),
    })
}

#[cfg(test)]
mod party_tests {
    use ::world::group::{MemberStatus, Party, PartyMember};

    fn member(name: &str, guid: u64, status: u8) -> PartyMember {
        PartyMember {
            name: name.to_string(),
            guid,
            status,
            subgroup: 0,
            flags: 0,
            roles: 0,
        }
    }

    fn party(members: Vec<PartyMember>, leader: u64) -> Party {
        Party {
            group_type: 0,
            own_subgroup: 0,
            own_flags: 0,
            own_roles: 0,
            guid: 0x1f50,
            counter: 1,
            members,
            leader,
            loot: None,
        }
    }

    /// A member the client has never seen and never had a stats packet about
    /// reaches the frame as a row of `None`s -- **not as a row of zeroes**.
    /// That distinction is the whole reason the vitals are `Option`s, and it
    /// is invisible one layer down: a `unwrap_or(0)` here would draw a party
    /// of people who all look dead, and nothing about the picture would say
    /// the numbers were invented.
    #[test]
    fn an_unreplicated_member_carries_no_numbers() {
        let mut state = ::world::WorldState::default();
        state.party = Some(party(
            vec![member("Watcher", 3, MemberStatus::ONLINE)],
            1,
        ));
        let rows = super::party_view(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Watcher", "the name comes from the packet");
        assert_eq!(rows[0].guid, 3);
        assert_eq!(rows[0].health, None, "an absent health became a zero");
        assert_eq!(rows[0].max_health, None);
        assert_eq!(rows[0].level, None);
        assert_eq!(
            rows[0].power_type, None,
            "an absent power type became mana"
        );
        assert!(rows[0].online);
        assert!(!rows[0].dead);
        assert!(!rows[0].leader, "the leader is guid 1, not this member");
    }

    /// What the party packet states about a member is drawn from the party
    /// packet, whatever the entity table does or does not hold: online, dead
    /// and leader are all in every group list, for every member, on every
    /// send.
    #[test]
    fn status_and_leadership_come_from_the_group_list() {
        let mut state = ::world::WorldState::default();
        state.party = Some(party(
            vec![
                member("Watcher", 3, MemberStatus::ONLINE),
                member("Huntertest", 4, MemberStatus::ONLINE | MemberStatus::DEAD),
                member("Testdruid", 5, 0),
            ],
            3,
        ));
        let rows = super::party_view(&state);
        assert!(rows[0].leader, "the leader's own row must say so");
        assert!(!rows[1].leader);

        assert!(rows[1].dead, "a dead member read as alive");
        assert!(rows[1].online, "dead is not offline");

        assert!(!rows[2].online, "an offline member read as connected");
        assert!(
            !rows[2].dead,
            "offline is not dead -- a shared flag would make waiting pointless"
        );
    }

    /// No group is no rows, and it must not be one blank row: an empty frame
    /// and a frame with a member nothing is known about would otherwise be
    /// the same picture.
    #[test]
    fn no_group_is_no_rows() {
        let state = ::world::WorldState::default();
        assert!(super::party_view(&state).is_empty());
    }
}

/// One name query waiting to be sent.
pub enum NameRequest {
    Player { guid: u64 },
    Creature { entry: u32, guid: u64 },
    /// **What a game object *is*, not what it is called.**
    ///
    /// The name is a by-product; the reason to ask is the type, which is the
    /// only thing that separates a mailbox from a bench. A display id draws
    /// either one and says nothing about which -- see
    /// `world::query::GameObjectInfo`.
    GameObject { entry: u32, guid: u64 },
}

/// Item entries worth asking about, and marking them asked.
///
/// The same shape as [`names_to_ask`] and for the same reasons: the cache
/// refuses to ask twice, so this is safe to call every frame, and `limit`
/// only stops a freshly-opened bag firing thirty packets in one frame.
///
/// **What is in a bag is not what is in range**, so this walks the
/// inventory rather than replicated entities: the player's own slots, plus
/// the contents of every equipped container. `extra` carries entries that
/// are on screen without being owned -- a loot window's rows, a vendor's
/// stock -- which is the same job `extra_players` does for chat senders who
/// were never in visibility range.
pub fn items_to_ask(
    state: &mut ::world::WorldState,
    player_guid: u64,
    extra: &[u32],
    limit: usize,
) -> Vec<u32> {
    let now = std::time::Instant::now();
    let held = ::world::inventory::held(state, player_guid);
    let mut wanted: Vec<u32> = held.iter().filter_map(|item| item.entry).collect();
    for bag in held.iter().filter(|item| item.capacity.is_some()) {
        wanted.extend(
            ::world::inventory::bag_contents(state, *bag)
                .into_iter()
                .flatten()
                .filter_map(|item| item.entry),
        );
    }
    wanted.extend_from_slice(extra);

    let mut asking = Vec::new();
    for entry in wanted {
        if asking.len() >= limit {
            break;
        }
        if entry != 0 && state.names.claim_item(entry, now) {
            asking.push(entry);
        }
    }
    asking
}

/// The volume a click has to pass through to select an entity.
///
/// Axis-aligned and square in `x`/`y`, sized by the model's widest horizontal
/// extent. Entities rotate about `Z` as they walk, and a box that turned with
/// them would need rebuilding every frame for no benefit -- a square one
/// contains every rotation of the model already, and erring towards easy to
/// click is the right error here.
pub fn hit_box(position: Vec3, scale: f32, bounds: Option<(Vec3, Vec3)>) -> (Vec3, Vec3) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (radius, low, high) = match bounds {
        Some((min, max)) => (
            [min.x.abs(), max.x.abs(), min.y.abs(), max.y.abs()]
                .into_iter()
                .fold(0.0f32, f32::max),
            min.z,
            max.z,
        ),
        None => (0.0, 0.0, 0.0),
    };

    let radius = (radius * scale).max(MIN_RADIUS);
    let low = low * scale;
    let high = (high * scale).max(low + MIN_HEIGHT);
    (
        position + Vec3::new(-radius, -radius, low),
        position + Vec3::new(radius, radius, high),
    )
}

/// Where the selected unit is on screen, as the box the click was tested
/// against.
///
/// Built from [`hit_box`] rather than from a second measurement of the model,
/// so the marker cannot drift away from what picking actually uses. If the two
/// ever disagree, the marker is the thing that shows it -- which is the point.
///
/// `None` if any corner is behind the camera. A partial projection is worse
/// than none: the perspective divide folds a point behind you back into view,
/// and the bracket would stretch across the screen following nothing.
pub fn marker_rect(
    camera: &render::camera::Camera,
    viewport: (f32, f32),
    position: Vec3,
    scale: f32,
    bounds: Option<(Vec3, Vec3)>,
) -> Option<egui::Rect> {
    let (min, max) = hit_box(position, scale, bounds);
    let mut screen_min = (f32::MAX, f32::MAX);
    let mut screen_max = (f32::MIN, f32::MIN);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let (x, y) = camera.project(corner, viewport)?;
        screen_min = (screen_min.0.min(x), screen_min.1.min(y));
        screen_max = (screen_max.0.max(x), screen_max.1.max(y));
    }
    Some(egui::Rect::from_min_max(
        egui::pos2(screen_min.0, screen_min.1),
        egui::pos2(screen_max.0, screen_max.1),
    ))
}

/// Where a floating combat-text number starts, in screen space.
///
/// `marker_rect`'s same trick applied to a single point instead of a box: a
/// damage number is spawned from one fixed world position (the victim's
/// position when the swing landed, not tracked to the unit afterwards -- a
/// killing blow's number must still finish rising even after the corpse it
/// came from is gone) and re-projected fresh every frame as the camera moves.
/// `None` behind the camera, the same reasoning `marker_rect` gives: a
/// perspective divide would fold the point back into view and the number
/// would jump across the screen following nothing.
pub fn combat_text_anchor(
    camera: &render::camera::Camera,
    viewport: (f32, f32),
    position: Vec3,
) -> Option<egui::Pos2> {
    let (x, y) = camera.project(position, viewport)?;
    Some(egui::pos2(x, y))
}

/// Where the player's own corpse should be bracketed, as a small fixed-size
/// box around its projected position.
///
/// `marker_rect`'s box comes from the model's own bounds, which a corpse has
/// none of readily to hand -- it is not a streamed, animated entity with a
/// display id looked up through the model cache, just a point the server
/// answered `MSG_CORPSE_QUERY` with. A fixed screen-space size is the same
/// trade `combat_text_anchor` already makes for a single point, just wrapped
/// in a small rectangle so [`frames::marker::draw`](ui::frames::marker::draw)
/// has ticks to draw.
pub fn corpse_marker_rect(
    camera: &render::camera::Camera,
    viewport: (f32, f32),
    position: Vec3,
) -> Option<egui::Rect> {
    const HALF_EXTENT: f32 = 20.0;
    let centre = combat_text_anchor(camera, viewport, position)?;
    Some(egui::Rect::from_min_max(
        egui::pos2(centre.x - HALF_EXTENT, centre.y - HALF_EXTENT),
        egui::pos2(centre.x + HALF_EXTENT, centre.y + HALF_EXTENT),
    ))
}

/// What a ray selects, if anything.
///
/// Nearest hit wins, so a creature standing in front of another takes the
/// click. `bounds_of` is passed in rather than looked up because the model
/// cache lives in the streaming world and this has no business reaching into
/// it.
pub fn pick(
    ray: &Ray,
    entities: &[live::Entity],
    bounds_of: &dyn Fn(u32) -> Option<(Vec3, Vec3)>,
) -> Option<u64> {
    let mut best: Option<(f32, u64)> = None;
    for entity in entities {
        let (min, max) = hit_box(entity.position, entity.scale, bounds_of(entity.display_id));
        let Some(distance) = ray.hits_box(min, max) else {
            continue;
        };
        if best.is_none_or(|(nearest, _)| distance < nearest) {
            best = Some((distance, entity.guid));
        }
    }
    best.map(|(_, guid)| guid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(guid: u64, position: Vec3) -> live::Entity {
        live::Entity {
            guid,
            display_id: 1,
            position,
            orientation: 0.0,
            scale: 1.0,
            kind: ::world::ObjectType::Unit,
            level: Some(1),
            speed: 0.0,
            turning: 0.0,
            airborne: false,
            swimming: false,
            dead: false,
            died_ms_ago: None,
            swung_ms_ago: None,
            casting_spell: None,
            cast_landed: None,
            fighting: false,
            appearance: None,
            visible_items: [0; ::world::inventory::EQUIPPED_COUNT as usize],
            sheathed: true,
            sheath_changed_ms_ago: None,
        }
    }

    fn bounds_of(_: u32) -> Option<(Vec3, Vec3)> {
        Some((Vec3::new(-0.4, -0.9, 0.0), Vec3::new(0.4, 0.9, 2.2)))
    }

    /// A model reporting nothing useful about its own size still has to be
    /// clickable, or a handful of creatures are permanently unselectable and
    /// the cause is a field nobody thinks to look at.
    #[test]
    fn a_model_with_no_bounds_still_gets_a_clickable_box() {
        let (min, max) = hit_box(Vec3::ZERO, 1.0, None);
        assert!(max.x - min.x >= MIN_RADIUS * 2.0);
        assert!(max.z - min.z >= MIN_HEIGHT);

        let (min, max) = hit_box(Vec3::ZERO, 1.0, Some((Vec3::ZERO, Vec3::ZERO)));
        assert!(max.x - min.x >= MIN_RADIUS * 2.0);
        assert!(max.z - min.z >= MIN_HEIGHT);
    }

    /// The box has to contain the model in every rotation about `Z`, because
    /// it is not rebuilt as the model turns. A long, narrow creature that
    /// turned side-on would otherwise become unclickable half the time.
    #[test]
    fn the_hit_box_is_square_so_turning_cannot_shrink_it() {
        let (min, max) = hit_box(Vec3::ZERO, 1.0, bounds_of(1));
        assert_eq!(max.x - min.x, max.y - min.y);
        // Wide enough for the model's longest horizontal axis, not its
        // shortest.
        assert!((max.x - min.x) >= 1.8 - f32::EPSILON);
    }

    #[test]
    fn scale_grows_the_hit_box() {
        let (small_min, small_max) = hit_box(Vec3::ZERO, 1.0, bounds_of(1));
        let (big_min, big_max) = hit_box(Vec3::ZERO, 2.0, bounds_of(1));
        assert!((big_max.x - big_min.x) > (small_max.x - small_min.x));
        assert!((big_max.z - big_min.z) > (small_max.z - small_min.z));
    }

    /// A scale of zero arrives from the wire as an unset field, and would
    /// otherwise collapse the box to a point.
    #[test]
    fn a_degenerate_scale_is_treated_as_normal() {
        let (min, max) = hit_box(Vec3::ZERO, 0.0, bounds_of(1));
        let (normal_min, normal_max) = hit_box(Vec3::ZERO, 1.0, bounds_of(1));
        assert_eq!((min, max), (normal_min, normal_max));
    }

    /// Two creatures on the same line: the click takes the near one.
    #[test]
    fn the_nearest_entity_takes_the_click() {
        let ray = Ray {
            origin: Vec3::new(-10.0, 0.0, 1.0),
            direction: Vec3::X,
        };
        let entities = [
            entity(2, Vec3::new(20.0, 0.0, 0.0)),
            entity(1, Vec3::new(5.0, 0.0, 0.0)),
        ];
        assert_eq!(pick(&ray, &entities, &bounds_of), Some(1));
    }

    /// And a click at nothing selects nothing, rather than the closest thing
    /// in the general direction.
    #[test]
    fn a_ray_through_empty_space_selects_nothing() {
        let ray = Ray {
            origin: Vec3::new(-10.0, 0.0, 1.0),
            direction: Vec3::X,
        };
        let entities = [entity(1, Vec3::new(5.0, 40.0, 0.0))];
        assert_eq!(pick(&ray, &entities, &bounds_of), None);

        // Behind the viewer is also nothing, however well aligned.
        let entities = [entity(1, Vec3::new(-40.0, 0.0, 0.0))];
        assert_eq!(pick(&ray, &entities, &bounds_of), None);
    }

    /// A speaker is named from the cache at *render* time, not at arrival.
    ///
    /// That distinction is the whole reason the viewer keeps the messages that
    /// arrived rather than the lines they rendered to: a whisper comes from
    /// someone who may be nowhere near, so they were never in replicated state
    /// to be name-queried, and the query only goes out *because* the message
    /// arrived. Rendering once on arrival stamps the guid in permanently, and
    /// the name that turns up a moment later has nowhere to go.
    #[test]
    fn a_speaker_is_named_from_the_cache_when_the_line_is_rendered() {
        let message = ::world::ChatMessage {
            chat_type: ::world::ChatType::Whisper,
            language: 7,
            sender: 0x35,
            sender_name: None,
            target: 0x32,
            channel: None,
            text: "hello".into(),
            tag: 0,
        };

        let mut state = ::world::WorldState::new();
        // Before the answer: the guid is all there is.
        let bare = chat_entry(&message, &state);
        assert_eq!(bare.who.as_deref(), Some("35"));

        state.names.apply_player(&::world::PlayerName {
            guid: 0x35,
            name: Some("Watcher".into()),
            realm: String::new(),
            race: 1,
            gender: 0,
            class: 1,
        });

        // The very same message, rendered again, now knows who said it.
        let named = chat_entry(&message, &state);
        assert_eq!(named.who.as_deref(), Some("Watcher"));
        assert_eq!(named.rendered(), "[whisper] Watcher: hello");
    }

    /// A creature names itself in the packet, and must be believed over the
    /// cache -- it may be dead before any query could be answered.
    #[test]
    fn an_inline_name_wins_over_the_cache() {
        let message = ::world::ChatMessage {
            chat_type: ::world::ChatType::MonsterSay,
            language: 0,
            sender: 0xF130_0000_2B00_0BBA,
            sender_name: Some("Young Wolf".into()),
            target: 0,
            channel: None,
            text: "growls".into(),
            tag: 0,
        };
        let state = ::world::WorldState::new();
        assert_eq!(chat_entry(&message, &state).who.as_deref(), Some("Young Wolf"));
    }

    /// A party line has to read as party, not fall into the `Other` bucket
    /// every unhandled chat type lands in -- see `chat_kind`'s match arm.
    #[test]
    fn a_party_line_is_named_and_coloured_as_party() {
        let message = ::world::ChatMessage {
            chat_type: ::world::ChatType::Party,
            language: 7,
            sender: 0x35,
            sender_name: None,
            target: 0,
            channel: None,
            text: "on my way".into(),
            tag: 0,
        };
        let mut state = ::world::WorldState::new();
        state.names.apply_player(&::world::PlayerName {
            guid: 0x35,
            name: Some("Watcher".into()),
            realm: String::new(),
            race: 1,
            gender: 0,
            class: 1,
        });
        let entry = chat_entry(&message, &state);
        assert_eq!(entry.kind, ui::ChatKind::Party);
        assert_eq!(entry.rendered(), "[party] Watcher: on my way");
    }

    /// The same for guild, and this one is a live bug converted rather than a
    /// precaution: 4.28 shipped able to *send* a guild line and unable to
    /// *draw* one. `ChatType::Guild` parsed correctly the whole time and both
    /// maps in this file simply had no arm for it, so every guild line drew
    /// with no tag in `Other`'s grey -- which beside `Say`'s near-white is a
    /// difference nobody at the window can see, and the report that came back
    /// was "`/g` does not stick". The send path was never involved.
    ///
    /// Both halves are asserted, because the tag alone would pass with the
    /// colour still wrong and the colour alone would pass with no tag.
    #[test]
    fn a_guild_line_is_named_and_coloured_as_guild() {
        let message = ::world::ChatMessage {
            chat_type: ::world::ChatType::Guild,
            language: 7,
            sender: 0x35,
            sender_name: None,
            target: 0,
            channel: None,
            text: "anyone for the abbey".into(),
            tag: 0,
        };
        let mut state = ::world::WorldState::new();
        state.names.apply_player(&::world::PlayerName {
            guid: 0x35,
            name: Some("Watcher".into()),
            realm: String::new(),
            race: 1,
            gender: 0,
            class: 1,
        });
        let entry = chat_entry(&message, &state);
        assert_eq!(entry.kind, ui::ChatKind::Guild);
        assert_eq!(entry.rendered(), "[guild] Watcher: anyone for the abbey");

        let style = ui::Style::default();
        // The bug's actual shape: `Other` and `Say` are both near-neutral, so
        // a kind that falls through to either is invisible rather than wrong.
        assert_ne!(
            ui::frames::chat::colour(entry.kind, &style),
            ui::frames::chat::colour(ui::ChatKind::Other, &style)
        );
        assert_ne!(
            ui::frames::chat::colour(entry.kind, &style),
            ui::frames::chat::colour(ui::ChatKind::Say, &style)
        );
    }

    /// A guild line from a **game master** takes the other parser -- every
    /// account on this project's own realm is one -- and has to arrive as
    /// guild all the same. This was the standing hypothesis for the live
    /// report and turned out not to be the cause; it is asserted anyway,
    /// because `SMSG_GM_MESSAGECHAT` really does carry a different body and
    /// the thing that must survive it is the `ChatType`.
    #[test]
    fn a_game_masters_guild_line_is_still_guild() {
        let mut body = Vec::new();
        body.push(::world::ChatType::Guild.id());
        body.extend_from_slice(&7u32.to_le_bytes());
        body.extend_from_slice(&1u64.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        // A GM's line names its sender inline, monster-shaped: length
        // counting the terminator, then the name.
        body.extend_from_slice(&9u32.to_le_bytes());
        body.extend_from_slice(b"Testwolf\0");
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&5u32.to_le_bytes());
        body.extend_from_slice(b"hello\0");
        body.push(0);

        let message = ::world::chat::parse_gm_message_chat(&body)
            .expect("a gm guild line should parse whole");
        assert_eq!(message.chat_type, ::world::ChatType::Guild);
        let entry = chat_entry(&message, &::world::WorldState::new());
        assert_eq!(entry.kind, ui::ChatKind::Guild);
        assert_eq!(entry.rendered(), "[guild] Testwolf: hello");
    }

    fn camera() -> render::camera::Camera {
        render::camera::Camera::Fly(render::camera::Fly {
            position: Vec3::new(-10.0, 4.0, 3.0),
            yaw: 0.0,
            pitch: 0.0,
            ..render::camera::Fly::default()
        })
    }

    /// A point in front of the camera lands on screen; the same point pushed
    /// behind it must not, or a killing blow's number would flash into view
    /// mirrored the instant the camera looked away.
    #[test]
    fn combat_text_anchor_follows_marker_rects_own_rule_about_the_camera() {
        let camera = camera();
        const VIEWPORT: (f32, f32) = (1600.0, 900.0);
        assert!(combat_text_anchor(&camera, VIEWPORT, Vec3::new(20.0, 4.0, 3.0)).is_some());
        assert!(combat_text_anchor(&camera, VIEWPORT, Vec3::new(-40.0, 4.0, 3.0)).is_none());
    }

    /// The interface draws zeroes for fields that have not arrived, rather
    /// than refusing to draw the frame.
    #[test]
    fn a_unit_with_no_fields_yet_still_makes_a_view() {
        let entity = ::world::state::Entity {
            guid: 5,
            object_type: ::world::ObjectType::Unit,
            position: None,
            fields: ::world::update::Fields::default(),
            movement: None,
            destination: None,
            move_duration: None,
            move_started: None,
            arrival_facing: None,
            path_facing: ::world::state::PathFacing::default(),
            last_move_time: None,
            died_at: None,
            last_swing: None,
            last_cast: None,
            updates: 0,
            sheath_changed_at: None,
        };
        let view = unit_view(&entity, "Creature".into());
        assert_eq!(view.health, 0);
        assert_eq!(view.max_health, 0);
        assert!(!view.has_power(), "no power type means no power bar");
        assert_eq!(view.health_fraction(), 0.0);
    }

    fn a_party_of_watcher() -> ::world::state::WorldState {
        let mut state = ::world::state::WorldState::default();
        state.party = Some(::world::group::Party {
            group_type: 0,
            own_subgroup: 0,
            own_flags: 0,
            own_roles: 0,
            guid: 0x1f50,
            counter: 1,
            members: vec![::world::group::PartyMember {
                name: "Watcher".into(),
                guid: 3,
                status: ::world::group::MemberStatus::ONLINE,
                subgroup: 0,
                flags: 0,
                roles: 0,
            }],
            leader: 1,
            loot: None,
        });
        state
    }

    /// Not a party member at all -- an old target from a group that has
    /// since disbanded, say -- draws nothing rather than an empty frame.
    #[test]
    fn party_target_view_is_none_for_a_stranger() {
        let state = ::world::state::WorldState::default();
        assert!(party_target_view(&state, 3).is_none());
    }

    /// The ordinary case: a party member with a full stats packet on file
    /// gives the target frame everything it needs, unreplicated or not.
    #[test]
    fn party_target_view_reads_the_party_packet() {
        let mut state = a_party_of_watcher();
        state.party_stats.insert(
            3,
            ::world::group::MemberStats {
                guid: 3,
                mask: 0,
                status: None,
                health: Some(60),
                max_health: Some(60),
                power_type: Some(1),
                power: Some(0),
                max_power: Some(1000),
                level: Some(1),
                zone: Some(12),
                position: None,
            },
        );
        let view = party_target_view(&state, 3).expect("Watcher is a party member");
        assert_eq!(view.name, "Watcher");
        assert_eq!(view.health, 60);
        assert_eq!(view.max_health, 60);
        assert_eq!(view.max_power, 1000);
        assert!(view.has_power());
        assert_eq!(view.power_type, ui::PowerType::Rage, "type 1 is rage, not mana");
    }

    /// **The case this function exists to get right.** A `max_power` can
    /// arrive before the `power_type` that says what it is -- the two are
    /// independent fields in a mask-driven packet -- and drawing a bar in
    /// that state would silently colour it mana, which is the exact bug
    /// `PartyVitals::power_type` was made an `Option` to prevent. No known
    /// type has to mean no bar, not a guessed one.
    #[test]
    fn an_unknown_power_type_draws_no_bar_even_with_a_known_maximum() {
        let mut state = a_party_of_watcher();
        state.party_stats.insert(
            3,
            ::world::group::MemberStats {
                guid: 3,
                mask: 0,
                status: None,
                health: Some(60),
                max_health: Some(60),
                power_type: None,
                power: Some(0),
                max_power: Some(1000),
                level: Some(1),
                zone: Some(12),
                position: None,
            },
        );
        let view = party_target_view(&state, 3).expect("Watcher is a party member");
        assert!(
            !view.has_power(),
            "a maximum with no known type drew a bar anyway"
        );
    }
}
