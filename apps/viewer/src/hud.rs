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
    }
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
    asking
}

/// One name query waiting to be sent.
pub enum NameRequest {
    Player { guid: u64 },
    Creature { entry: u32, guid: u64 },
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
            moving: false,
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
            updates: 0,
        };
        let view = unit_view(&entity, "Creature".into());
        assert_eq!(view.health, 0);
        assert_eq!(view.max_health, 0);
        assert!(!view.has_power(), "no power type means no power bar");
        assert_eq!(view.health_fraction(), 0.0);
    }
}
