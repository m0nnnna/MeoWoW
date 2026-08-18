# The interface

## The decision

**This client draws its own interface. It does not reimplement `FrameXML`, and
it does not run addons.**

3.3.5a's interface is Lua 5.1 driving an XML-defined frame tree. Addon
compatibility is not a matter of looking similar -- it means reproducing that
widget system faithfully enough that third-party code written against it keeps
working: the frame hierarchy, the event names and their argument order, the
templates, the taint rules, the secure-frame restrictions, and a Lua runtime to
host it all. That is a large subsystem in its own right, and none of it draws a
single health bar until most of it exists.

The trade taken here is the other one. The interface is native, addons are given
up, and the thing paid back is that **the interface itself is the customisation
surface**. Every position, size, colour and dimension lives in one text file the
user owns, and the same values can be dragged around inside the running client.
There is no fixed appearance to patch around, because there is no appearance
that is not a value in that file.

What is genuinely lost: the enormous existing body of 3.3.5a addons. If that
matters more than everything else, this is the wrong client. What is not lost is
customisation, which is what most people actually wanted addons for.

## What egui is doing, and what it is not

egui is the drawing and input substrate: a font atlas, a pointer, a
tessellator, and a place to put windows. It passes this project's reuse test
(`docs/REUSE-POLICY.md`) easily -- it would exist if WoW never had.

It is **not** the interface's look, and this distinction is load-bearing:

- Frames are painted from explicit geometry -- `rect_filled`, `text`, a
  `Painter` and a clip rect -- not assembled from egui widgets.
- Consequently `scale` genuinely multiplies every dimension of a frame, because
  every dimension is a number this crate computed.
- Consequently the appearance is a function of `Style` alone. Rewriting the
  style file gives a different-looking client, not egui with different colours.

egui's own windows are still used for the things that *are* tooling: the debug
overlay, and the layout editor. Those are meant to look like tools.

## The pieces

`crates/ui`:

| Module | What it owns |
|---|---|
| `element` | Where a frame sits: anchor, offset, scale, visibility |
| `style` | Every dimension and colour the frames draw with |
| `layout` | The whole profile, and the file it lives in |
| `frames` | The frames themselves: `unit`, `party`, `party_invite`, `chat`, `action_bar`, `cast_bar`, `spellbook`, `marker`, `combat_text` |
| `edit` | Rearranging it from inside the running client |
| `Hud` | What a caller holds: profile + edit state + the draw call |

`apps/viewer/src/hud.rs` is the bridge: replicated `world` state becomes plain
`ui::UnitView` snapshots going out, and a click becomes a target coming in. The
`ui` crate deliberately depends on neither `world` nor `render`, so it can be
tested without a connection or a GPU -- and is.

## Anchoring

Every element is placed by one rule: pick a point on the screen (the anchor),
pick the same-named point on the element, and offset one from the other.

That is what makes a layout survive a resize. An element anchored bottom-right
stays in the corner when the window grows, where a stored absolute position
would drift off the edge or leave a gap.

The rule runs in both directions, and **that is the part worth being careful
about**. Drawing needs anchor + offset to become a rectangle (`Element::rect`);
dragging needs a rectangle to become an offset (`Element::offset_for`). Two
separately written conversions drift, and the symptom -- a frame that creeps a
few pixels every time it is dragged -- is slow enough to blame on the pointer.
They are written as one formula and its inverse and round-tripped in the tests,
which is the same defence this project applies to any structure that travels
both ways (`CLAUDE.md`: "writing a format is riskier than reading it").

Re-anchoring does not move a frame. `Element::rebase` recomputes the offset so
the pixels stay put and only the reference changes -- so the difference shows up
on the next resize, which is when the user meant it to. Changing an anchor and
having the frame teleport across the window reads as a bug, not as a choice.

## The layout file

`%APPDATA%\open-wow\ui.toml` on Windows; `$XDG_CONFIG_HOME/open-wow/ui.toml` or
`~/.config/open-wow/ui.toml` elsewhere. Probed by environment rather than by
`cfg!(windows)`, so the same code answers correctly under either.

```toml
[style]
frame_width = 230.0
bar_height = 18.0
health = "#3aa84a"
background = "#101218d2"
show_values = true

[elements.player-frame]
anchor = "top-left"
offset = [24.0, 24.0]
scale = 1.0
visible = true

[elements.target-frame]
anchor = "bottom-right"
offset = [-40.0, -120.0]
```

Colours are `#rrggbb` or `#rrggbbaa` strings rather than arrays of numbers,
because the audience for this file is a person with a colour picker open.

Reading it is **forgiving in one direction and strict in the other**:

- Fields left out inherit their defaults, so hand-editing one value is safe and
  does not require discovering the full field list.
- An element this build has never heard of is *reported and dropped*, not
  refused. A layout written by a later build must not stop this one starting.
- A malformed value (`scale = "big"`) is an error. Silently substituting a
  default teaches the user that the file is being read when it is not.
- A value that would make the interface unusable (`scale = 100`) is clamped and
  said so, because the controls that would undo it would be underneath it.

Whatever had to be corrected lands in `Hud::status` and on screen, not only in
the log. A customisation that did not take effect has to say so where the
customising is happening.

Saving writes to a sibling file and renames over the target. This project has
already destroyed one file by writing it non-atomically and failing partway
(`CLAUDE.md`, "Traps already hit"); the same failure here would cost a user
their layout at the moment they tried to save it.

## Edit mode

`F1` in the viewer. Frames become draggable in place, snapping to a grid, and a
window exposes anchor, offset, scale, visibility and the most-wanted style
values, with save / reload / reset.

Two details that are not obvious:

- **A frame with nothing to show still draws while editing**, filled with
  plausible numbers. Otherwise the target frame could only be positioned while
  something was targeted -- and it would have to stay targeted for the whole
  drag, which is not a thing that can be asked of a user.
- **The file and the editor are the same profile.** Neither is the "real" route.
  A text file alone is a poor way to answer "is that health bar too big?"; the
  loop is edit, restart, look, repeat.

## Click-to-target

A left click that did not travel more than a few pixels is a click; the same two
events with movement between them is the drag that turns the camera. Nothing
else distinguishes them, so the distance is measured (`CLICK_SLOP`).

The ray comes from `Camera::ray_through`, unprojected from **the same
view-projection matrix the scene is drawn with** rather than rebuilt from the
camera's angles. The two agree only as long as nobody changes the projection,
and a picking ray that disagrees with the view by a little is much harder to
notice than one that disagrees by a lot: clicks land on the creature beside the
one under the cursor, which reads as the server disagreeing about positions.
The test projects a world point to a pixel and casts a ray back through it.

Hit volumes are axis-aligned boxes, square in `x`/`y`, sized from the model's
widest horizontal extent:

- Entities rotate about `Z` as they walk. A box that turned with them would need
  rebuilding every frame for no benefit; a square one already contains every
  rotation of the model.
- A minimum size applies whatever the model claims. Some models report a tiny or
  empty bounding box, and left alone those creatures would be visible and
  unclickable -- which reads as targeting being broken rather than as one
  model's metadata being thin.
- Erring towards easy-to-click is the right error for a target selector.

Selection is sent to the server as `CMSG_SET_SELECTION`. The interface could
keep a target to itself, but the server decides whether a spell or an attack has
a legal victim. Clicking empty ground clears it, which goes out as a guid of
zero rather than as no packet: the server holds the last selection it was given.

## Power, and a field that parses perfectly while being wrong

A unit's current power is **not** `UNIT_FIELD_POWER1`. The seven power fields
are a parallel array indexed by the unit's own power type, packed into byte 3 of
`UNIT_FIELD_BYTES_0`. Reading `POWER1` unconditionally is right for every caster
and reports zero for every rogue and warrior in the world -- which looks like a
replication failure rather than a misread field.

The index is bounds-checked, and the test says why in the sharpest available
terms: `UNIT_POWER1 + 29` lands *exactly* on `UNIT_LEVEL`, so an unguarded read
of an unfamiliar power type reports a unit's level as its current mana. A
plausible number, in the right kind of range, and wrong.

`wow-cli world --enter <name> --units <n>` dumps exactly what a unit frame
would read, which is the dump command this project's rules say should have come
before any of it was wired into a renderer. Against the live realm it says what
it should: `Testwolf` reads race 1, class 1, gender 0, power type 1, power
`0/1000` — a Human Warrior with rage, where 1000 is rage's known scaling. A
`POWER1`-regardless read would have shown `0/0`.

## Chat

The chat frame is the first thing here that has to **wrap**, so it is the first
that cannot be drawn from rectangles and single-line text. egui's galley layout
does the wrapping; what colour a line is, how many are kept and where the box
sits stay this crate's decisions.

Lines are laid out **from the bottom up**. Chat grows downward and the newest
line matters most, so laying out from the bottom means a partially visible line
is always the *oldest* one on screen — the one you can afford to lose. The loop
also stops as soon as the box is full, so a long scrollback costs no layout for
lines nobody can see.

`Enter` opens a line to type in and **takes the keyboard away from movement**
while it is open, which matters more than it sounds: without it, typing "we"
walks your character across the zone. Keys held at the moment chat opens are
cleared too, because every later key event is swallowed by the chat line and
nothing would ever release them.

Typed characters come from what the key actually **produced**, not from its
physical code — deriving a character from the code types QWERTY on an AZERTY
keyboard. Control characters are filtered, since Enter arrives as `"\r"` and
would otherwise be appended to the message and sent.

### A line is rendered every frame, not stored as text

The viewer keeps the chat messages that *arrived*, and re-renders each one to a
`ChatEntry` every frame. That looks wasteful and is the point: a whisper comes
from someone who may be nowhere near, so they were never in replicated state to
be name-queried — the query only goes out *because* the line arrived. Rendering
once on arrival stamped the guid in permanently, and the name that resolved a
moment later had nowhere to go. Found by reading the viewer's own log, which is
why received chat is logged at debug as well as drawn.

Local notices ("could not send: …") are stored as `System` messages from guid
zero rather than as a second kind of thing, so there is one path to render.

## Names

Frames say `Young Wolf`, not `Creature 299`. Two facts shape the cache
(`crates/world/src/names.rs`), and neither is about parsing:

- **Ask once.** A guid is recorded as outstanding when the query goes *out*,
  not when the answer arrives. The interface asks every frame; without this it
  would send sixty queries a second for the same wolf.
- **A refusal is an answer, silence is not.** "No such guid" is cached like any
  name, because asking again gets the same reply forever. A query that is never
  answered times out and becomes askable again, or one dropped packet leaves
  something nameless for the session.

Creature names are keyed by **entry**, not guid — 131 replicated objects cost
50 queries, because a zone of forty wolves shares one answer. The viewer sends
at most a handful per frame; the cache makes that safe to call every frame, and
the cap only stops a hundred packets going out in the frame after login.

## Cast bar

`crates/ui/src/frames/cast_bar.rs`. Fills **left to right** as a cast
completes -- deliberately the opposite direction from the action bar's
cooldown sweep, which darkens as a spell becomes ready again. The two bars
are measuring opposite things, and reading the same direction for both would
say "ready" and "casting" with the same shape.

It follows the target frame's rule, not the action bar's: **absent unless
relevant**, appearing only while `WorldState::active_cast` reports one, with
the same edit-mode placeholder every other conditionally-shown frame gets so
it can be positioned without waiting for a live cast.

`SMSG_SPELL_START` and `SMSG_SPELL_GO` back it, parsed against a real capture
from `wow1.nekos.farm` rather than transcribed from documentation and hoped
correct -- `docs/ROADMAP.md`'s "4.3 finished: cast bars" has the byte-level
story. Two things from that capture shape the parser's honesty, not just its
layout: even a self-cast named an explicit unit target rather than
`target_flags::SELF`, so any other target shape is refused by name instead of
misread; and because `SMSG_SPELL_GO` can therefore be refused for a shape
this parser has not confirmed (a miss, in particular), the bar cannot depend
on that packet arriving to disappear. `WorldState::active_cast` reads `None`
once a cast's own duration has elapsed regardless of whether
`SMSG_SPELL_GO` ever lands -- a stuck cast bar is not a failure mode this
client has.

## Spellbook, and arranging a bar from inside the client

`crates/ui/src/frames/spellbook.rs`, opened with `P`. It lists what the
character can do; a left click picks a spell up, a left click on an action
slot puts it down, and a right click on a slot empties it. The held spell is
drawn against the cursor, and Escape or a right click anywhere puts it back.

**Why it had to exist before combat could be tried.** Until it did, the bars
were filled once at login by `App::seed_action_bars` and could never be
changed from inside the client, so any ability the seeder's filter rejected
was unreachable no matter what the character knew. Auto-attack was exactly
that, and not by accident: `SkillLineAbility` puts spell 6603 on the generic
line 183 with a class mask of zero -- the same row shape as `Opening`,
`Closing`, `Duel` and `Honorless Target`, which is precisely the shape
`Spellbook::castable` exists to throw away. The mechanism that correctly
keeps a warrior's bar free of junk necessarily rejected the one ability every
character in the game uses.

That left two fixes and only one of them is right. Widening the filter to
admit line 183 readmits all the junk with it; naming the single spell admits
exactly what was checked. `spells::AUTO_ATTACK` is therefore a hardcoded id
with its evidence written next to it, and
`auto_attack_is_admitted_and_the_junk_beside_it_is_not` asserts *both* halves
against the real archives -- because a test of the first half alone would pass
just as well under the wrong fix.

**A slot still stores a plain spell id.** `ui.toml` is unchanged and a layout
written before any of this existed still loads. Which message a slot sends is
derived from the spell rather than stored beside it, so a bar arranged by hand
in the file behaves identically to one arranged in-game. Auto-attack is the
one spell that does not travel by `CMSG_CAST_SPELL`: it is a *state*, bracketed
by `SMSG_ATTACKSTART` and `SMSG_ATTACKSTOP`, so its slot toggles, and whether
it is currently on is read out of `WorldState::attacking` rather than kept as a
local flag. The server ends an attack by itself when the target dies or walks
out of range; a local flag would be inverted from that moment on, and the next
press would send a stop for a fight that was already over and look like the
key had failed.

**A hold does not outlive the book.** Closing the spellbook puts down whatever
was picked up, because the held indicator is drawn from the book's own entry --
a hold that survived would be a mode with nothing on screen to show it, and the
next click on a bar would silently mean "put" instead of "cast".

An assignment writes `ui.toml` immediately rather than waiting for edit mode's
Save button. Arranging a bar is not editing the layout: it happens mid-play
with the edit window shut, and a spell that has to be dragged on again after
every restart is worse than no spellbook at all.

## The camera is a saved setting, not a constant

`ui.toml` carries a `[camera]` section: how far a drag across the window turns
the view (in degrees, so the number in the file is one a person can picture),
how far back the camera starts, and whether the vertical axis is inverted. The
edit window (`F1`) has sliders for all three.

Camera preferences are not frames and this crate does not draw the camera, so
they sit here for one reason: **`Profile` is the thing that gets written to
disk**, and a setting a player changes is one they expect to still be there
tomorrow. Putting them in the viewer would have meant a second config file.

Two details are load-bearing:

- **The turn rate is per *window*, not per pixel.** The viewer's own constant
  used to be 0.008 radians a pixel, annotated "roughly half a turn across the
  window" -- which on a 1920-wide window is two and a half *full* turns, and
  worse on a larger monitor. Expressing it per window makes the feel a property
  of the gesture rather than of the display, and makes the setting mean the
  same thing on every machine.
- **`radians_per_pixel` clamps every time it is asked**, rather than the value
  being sanitised at load. `ui.toml` is meant to be hand-edited, so the number
  is an input from outside like any other, and a zero would freeze the camera
  while a negative would invert it in a way no setting says it should. Guarding
  at the point of use means a caller that built the struct some other way
  cannot skip it.

The distance range is exported from here (`camera::MIN_DISTANCE` and
`MAX_DISTANCE`) and the viewer's wheel clamps to those same two constants
rather than its own copy -- otherwise the slider and the wheel would agree
until somebody edited one of them.

## The party frame, and where "not known" reaches the screen

The party frame is the first thing in this interface drawn from a source that
routinely **has no answer**, and that fact shapes the whole of it.

Every other frame draws something replicated. A unit frame's subject is an
object in visibility range, so its health, power and level are facts; a field
that has not arrived yet reads zero for a moment and fills in on the next
update. A party member is not like that. **A member two zones away is not a
replicated object at all** -- no object, no fields, nothing -- and the only
thing the server sends about them is `SMSG_PARTY_MEMBER_STATS`, which may not
have arrived either. A name and a guid, and nothing else, is a real and common
state.

So `PartyMemberView` carries `Option`s where `UnitView` carries `u32`s, and
they stay `Option`s all the way to the painter:

- a bar whose maximum is unknown is drawn **empty and unlabelled**, never full;
- an unknown level prints **nothing**, not `0` and not `??`;
- a member with no known power *type* gets **no second bar at all**, rather
  than a mana-coloured one.

That last one is the case worth naming, because it is the one a reasonable
implementation gets wrong. Health falls back between two sources holding the
same quantity, so `unwrap_or(0)` there is merely wrong. A power type is an
**index**: defaulting it to zero does not mean "not known", it means mana, so
a rogue whose stats packet has not arrived is drawn with a blue bar and nothing
anywhere says the colour was a guess. Same rule as the tooltip substituter
passing `$s1` through with its `$` intact -- a visible blank says "not known"
and a fabricated number says nothing and is believed.

`world::WorldState::party_member_vitals` is where the two sources meet, and the
order is not a preference: the replicated entity wins where there is one,
because a member in view is exact and current and the party summary is coarse
and can be a minute old. It falls back rather than going blank when the party
spreads out, which is when a party frame is actually for. The one field that
never falls back is the **zone**, which only the party packet carries -- a
player you can see is by definition in your zone, so the object has no such
field to lose it to.

Measured live on 2026-08-18, and kept as the bytes: `Watcher`, 186 units away
and unreplicated, read `60/60` health, `0/1000` rage, level 1, zone 12 --
every number from the party packet, and agreeing with what that character's own
client independently reported about itself. `crates/world/src/state.rs` holds
that capture and asserts both halves: the fallback, and the entity overriding
it.

### The rows are not all the same height

A member with a power bar is taller than one without, and a member out of view
has no power fields at all -- so a mixed party is the normal case, not the edge
one. `frames::party::row_at` therefore walks the same accumulating heights
`draw` does instead of dividing by an average. A uniform division puts the
click on the wrong person, silently, and "silently" is the whole problem: a
party frame's click *targets* somebody, so the failure is a spell cast on the
wrong member rather than an error.

### The invite prompt has two answers, which nothing else here does

Every other clickable frame either has rows (`loot`, `quest_log`, `bags`) or is
one big button (`release`). The invite prompt is neither: Accept and Decline
are **opposite** and sit side by side, so the geometry is stated once in
`frames::party_invite::buttons` and both `draw` and `click_at` read it. Two
separately written copies of the same rectangles agree right up until one of
them changes -- the same reasoning that makes the picking ray unproject the
matrix the scene is drawn with rather than rebuild it from the camera's angles.

A press that lands on neither button reports **nothing**, rather than the
nearer of the two. An accidental accept puts the character in a stranger's
group and has to be undone by leaving it; an ignored press costs nothing.

### Commands take `/`, the server takes `.`

`/invite`, `/leave`, `/kick` and `/promote` are handled in the viewer and never
reach the wire. A line beginning with `.` is a *server* command, parsed by the
realm's own chat handler, which is how this client sends GM commands today.
Sharing one prefix would mean guessing which end a line was meant for, and
guessing wrong says `/invite Watcher` out loud to everybody standing nearby.

Only the invite is answered. `SMSG_PARTY_COMMAND_RESULT` comes back whether it
worked or not, so that one says nothing locally and lets the reply speak;
every other party request is silent, so each says locally what it asked for.
Without that, a `/leave` sent while not in a group and a `/leave` the server
declined look identical -- which is the failure mode the whole `world::group`
block was written to escape.

## What is deliberately not here yet

- **Per-element style overrides.** One `Style` serves every frame. A second
  layer of per-element overrides is easy to add and easy to add badly; it waits
  until there are enough frames for the shape of the need to be visible.
- **The interface in `--screenshot`.** The headless path renders the scene
  without egui, so it cannot show the interface. Checking a frame's appearance
  needs a window, or the headless-egui tests in `crates/ui`, which run the real
  `Hud::show` and assert that something was painted at the rectangle the layout
  chose. That test exists because of a lesson already paid for here: geometry
  submitted at zero size looks exactly like geometry never submitted, and the
  search for it went to the wrong layer.
