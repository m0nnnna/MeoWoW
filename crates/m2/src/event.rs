//! Timed events: the moments inside an animation when something is supposed to
//! happen.
//!
//! Everything else this reader decodes is *continuous* -- a bone's position at
//! time `t`, an emitter's rate at time `t`. An event is the opposite: a bare
//! list of timestamps, with no value at all, saying "at 340ms into the run
//! cycle, this happened". The whole record is a four-character name, a bone to
//! hang the moment off, and one timestamp list per animation sequence.
//!
//! # Why this is worth parsing at all
//!
//! Two features in this client had to guess at a number that is in this block.
//! The weapon-impact delay is a constant dialled in by a person watching a
//! sword, documented as such, *because* "the time from the animation's first
//! frame to the frame the weapon connects" lives here and nothing read it. A
//! footstep has the same shape and is worse: a walk cycle's feet land at two
//! irregular moments per loop, so a client emitting footsteps on a timer plays
//! them out of step with the legs, which is precisely the sort of wrongness
//! that reads as the feature working badly rather than as a missing table.
//!
//! # The identifier is a name, not a number
//!
//! `identifier` is four ASCII bytes -- `$FL`, `$AH0`, `DEST` -- stored little
//! end first like every other word in the format, so it reads back reversed
//! unless it is treated as bytes. That is a gift by this project's standards:
//! a name cannot be arrived at by a coincidence of small integers, so a wrong
//! stride here does not produce plausible-looking events, it produces
//! punctuation. See [`Event::name`], and `wow-cli m2 events --strides`, which
//! is exactly that check run over every model in the archives.

/// Bytes per `M2Event` record.
///
/// **Measured rather than transcribed**, the same way the emitter strides
/// were, and the two checks agree. Byte accounting -- every timestamp array a
/// record points at landing past the end of the block -- fits **4,265 of
/// 4,265** models carrying events at 36, where 28 and 32 fit none and 40 and
/// 44 fit 1,343. The stronger check is the name: at 36, **25,498 of 25,500**
/// records have four printable identifier bytes, and every neighbouring stride
/// manages 21.7% to 23.6%, because it shifts the name into the middle of a
/// float. See `wow-cli m2 events --strides`.
pub const EVENT_SIZE: usize = 36;

/// One timed event on a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Four ASCII bytes naming what happens. Kept raw rather than as a
    /// `String` because a misparse must stay visible: `[0x00, 0xC8, 0x42, 0x00]`
    /// is obviously not a name, where a lossy conversion would hide it.
    pub identifier: [u8; 4],
    /// A per-event argument whose meaning depends on the identifier.
    ///
    /// **It identified itself, and it confirms the whole record layout.** The
    /// human male carries six `$CSD` events whose `data` values are 6576 and
    /// 6919-6923, and those are `SoundEntries` rows called `ClapSounds`,
    /// `HumanMaleEmoteChicken`, `HumanMaleEmoteCry`, `HumanMaleEmoteKiss` and
    /// `HumanMaleEmoteLaugh` -- sounds named for the very model carrying them.
    /// Each fires in exactly the sequence its name matches: `EmoteApplaud`
    /// (four times, at 634, 967, 1300 and 1634ms -- an applaud is four claps),
    /// `EmoteChicken`, `EmoteCry`, `EmoteKiss`, `EmoteLaugh`. No wrong stride
    /// and no wrong field offset produces sound names that match the animation
    /// names they fire in.
    ///
    /// Nothing here acts on it; emote sounds are a feature this client does
    /// not have.
    pub data: u32,
    /// The bone the event happens at, for the ones that have a place.
    pub bone: u32,
    /// Offset from that bone, in model space.
    pub position: [f32; 3],
    /// Timestamps, in milliseconds into the sequence, one list per animation
    /// sequence. Indexed exactly like [`crate::anim::Track`]'s outer array:
    /// entry `i` belongs to sequence `i`.
    pub times: Vec<Vec<u32>>,
}

impl Event {
    /// The identifier as text, with anything unprintable shown as `.`.
    ///
    /// Lossy on purpose. This is for reading, and the *count* of records whose
    /// name is clean is the measurement -- see [`Event::is_named`].
    pub fn name(&self) -> String {
        self.identifier
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect()
    }

    /// Whether all four identifier bytes are printable ASCII.
    ///
    /// This is the property that separates a right stride from a wrong one.
    pub fn is_named(&self) -> bool {
        self.identifier.iter().all(|&b| (0x20..0x7f).contains(&b))
    }

    /// Whether this is one of the two footfall markers.
    ///
    /// **The two names were read off the data, not recalled.** Over every
    /// model in the archives the identifier tally is dominated by `$FL` and
    /// `$FR`, they appear together on the models that have legs, and their
    /// timestamps in a walk cycle alternate. See `wow-cli m2 events --survey`.
    pub fn is_footfall(&self) -> bool {
        matches!(&self.identifier, b"$FL\0" | b"$FR\0")
    }

    /// The timestamps this event fires at during `sequence`, in milliseconds.
    ///
    /// An empty slice is the honest answer for a sequence the event does not
    /// occur in, which is most of them: a footfall belongs to the walk and run
    /// cycles and not to standing still.
    pub fn times_in(&self, sequence: usize) -> &[u32] {
        self.times.get(sequence).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The identifier of the ground-contact event: `$FSD`.
///
/// **Measured, and it had a rival that draws the same picture.** A model
/// carries two families that could be the footfall. `$FL0`/`$FR0` (and
/// `$RL0`/`$RR0` for the run, `$BL0`/`$BR0` for backing up) are one per foot,
/// each firing once per locomotion cycle, and sit at ground level offset to
/// opposite sides -- the left at +Y and the right at -Y by the same distance.
/// `$FSD` sits at the model's origin and fires more than once.
///
/// Three measurements, and they agree:
///
/// * **The count matches the number of legs.** The human male's walk cycle has
///   two `$FSD` timestamps; the wolf's has **four**, one per paw, and its run
///   has four as well. A marker that tracks how many feet a creature has is
///   the contact, not the cycle.
/// * **On the wolf they line up.** Each of the four `$FSD` times precedes its
///   matching per-foot event by exactly 33ms -- 34 before 67, 167 before 200,
///   534 before 567, 667 before 700 -- four times out of four.
/// * **On the human male the skeleton says the same thing.** Posing it through
///   the walk cycle, the two bones that reach the ground touch down at 255 and
///   755ms and the two that reach it lowest at 330 and 830 -- heel then toe.
///   `$FSD` fires at 266 and 800, between the pairs both times. `$FR0` fires at
///   0 and `$FL0` at 533, which is the middle of each foot's *stance*: they
///   mark where a foot is planted, not when it lands. The same holds in the run
///   (contacts at 246/580, `$FSD` at 267/600, `$RR0`/`$RL0` at 33/367).
///
/// `wow-cli m2 events <model> --trace <sequence>` is that experiment, and it
/// could have come out the other way.
pub const FOOTFALL: [u8; 4] = *b"$FSD";

/// When each sequence's feet hit the ground, one sorted list per sequence.
///
/// Empty lists are kept rather than skipped so the outer index stays a
/// sequence index: a caller holding sequence 137 must not have to know how
/// many of the sequences before it were silent.
pub fn footfalls(events: &[Event], sequences: usize) -> Vec<Vec<u32>> {
    let mut out = vec![Vec::new(); sequences];
    for event in events.iter().filter(|e| e.identifier == FOOTFALL) {
        for (sequence, times) in event.times.iter().enumerate() {
            let Some(slot) = out.get_mut(sequence) else {
                continue;
            };
            slot.extend_from_slice(times);
        }
    }
    for times in &mut out {
        times.sort_unstable();
        times.dedup();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(identifier: &[u8; 4], times: Vec<Vec<u32>>) -> Event {
        Event {
            identifier: *identifier,
            data: 0,
            bone: 0,
            position: [0.0; 3],
            times,
        }
    }

    /// Only the ground-contact events count, and the outer index stays a
    /// sequence index whether or not that sequence has any.
    #[test]
    fn footfalls_are_indexed_by_sequence() {
        let events = vec![
            event(b"$FSD", vec![vec![800, 266], Vec::new(), vec![34]]),
            // The per-foot markers are a different question -- where a foot is
            // planted, not when it lands -- and must not be counted.
            event(b"$FL0", vec![vec![533], Vec::new(), Vec::new()]),
            event(b"$CSD", vec![vec![0], Vec::new(), Vec::new()]),
        ];
        let times = footfalls(&events, 3);
        assert_eq!(times.len(), 3);
        assert_eq!(times[0], vec![266, 800], "sorted, and the per-foot marker excluded");
        assert!(times[1].is_empty(), "a sequence with no contacts keeps its slot");
        assert_eq!(times[2], vec![34]);
    }

    /// A model whose events name more sequences than it has does not grow the
    /// list past the sequence count, and one with fewer still gets a slot per
    /// sequence.
    #[test]
    fn the_list_is_always_one_slot_per_sequence() {
        let events = vec![event(b"$FSD", vec![vec![10], vec![20], vec![30]])];
        assert_eq!(footfalls(&events, 2), vec![vec![10], vec![20]]);
        assert_eq!(footfalls(&events, 5).len(), 5);
    }

    /// Two feet landing at the same instant is one contact, not two: a
    /// quadruped's front and rear paw can share a timestamp and playing the
    /// file twice at once is a doubled sound rather than a louder one.
    #[test]
    fn a_shared_timestamp_is_one_contact() {
        let events = vec![
            event(b"$FSD", vec![vec![100, 400]]),
            event(b"$FSD", vec![vec![100, 700]]),
        ];
        assert_eq!(footfalls(&events, 1), vec![vec![100, 400, 700]]);
    }

    /// An identifier is four printable bytes; anything else is a misparse and
    /// must stay visible rather than being cleaned up into a plausible name.
    #[test]
    fn a_misparsed_identifier_reads_as_punctuation() {
        assert_eq!(event(b"$FSD", Vec::new()).name(), "$FSD");
        assert!(event(b"$FSD", Vec::new()).is_named());
        let broken = event(&[0x00, 0xC8, 0x42, 0x00], Vec::new());
        assert_eq!(broken.name(), "..B.");
        assert!(!broken.is_named());
    }
}
