# open-wow-client

Open-source reimplementation of the WoW 3.3.5a (build 12340) client in Rust.
Client only — no server, no bundled assets.

## Where the project is

Phases 1, 2 and 3 are complete: every data format reads, the world renders and
streams, and the protocol reaches a live realm. Phase 4 has started.

| | State |
|---|---|
| Data formats | MPQ, DBC, BLP, M2 (+animation), WMO, ADT/WDT — all done |
| Renderer | Textures, skinned models, buildings, blended terrain, streaming — done |
| Protocol | **3.1–3.5 done**, all confirmed against a live realm including one client watching another move. Replicated *creatures* slide along their actual path, turn to face it, and play the model's own walk/stand cycles. **Other players do not** — see the known defect below |
| Interface | **4.1 and 4.2 done.** Native, fully customisable, no addons — see the decision below. Player and target unit frames, click-to-target with an in-world bracket, a chat window you can type in, real names, a spellbook you arrange the bars from, `F1` to rearrange, saved to `ui.toml` |
| World | Lighting and the day/night cycle come from `Light.dbc`'s curves and the realm's own clock: real sun, ambient and sky colour, dawn through midnight. Game objects — doors, benches, chests, ships — are drawn |
| Appearance | Humanoid NPCs wear their baked `CreatureDisplayInfoExtra` texture and other players are dressed from their replicated appearance fields, so nothing in a zone renders as a white ghost. The player's own armour is painted on from `ItemDisplayInfo`'s eight body components. **The player's weapon is drawn**: the M2 attachment table parses, and a sword or shield hangs off the hand's animated bone and swings with it. Shoulders, helms and ranged weapons are not, and there is no sheathed state — see below. Other players' equipment still needs their visible-item fields |
| Game | **4.3 done**: three action bars with real icons, keys `1`-`=` with Shift/Ctrl, click-to-cast, the player's own character drawn in third person with its chosen face, beard, skin and haircut, hover tooltips reading real numbers (82% of `Spell.dbc`'s description templates resolve), a cooldown sweep, and a cast bar off `SMSG_SPELL_START`/`SMSG_SPELL_GO`. **4.4 melee done**: swing at a target and be swung at, a named combat log (`You hit Kobold Vermin for 6. Killing blow.`), and a dead unit dimmed in the frames. **A spellbook panel** (`P`) now lists what the character can do and puts it on a bar by click, auto-attack included -- see the note below on why the seeding filter had to reject it. Threat and the corpse *interface* remain (the corpse protocol is done). Inventory and quests follow |

Roughly 58% of the way to something a person could test by playing. See
`docs/ROADMAP.md` for the milestone ladder and what is deliberately deferred.

**Weapons are drawn and sheathing does not exist.** `Item.dbc`'s
`sheathe_type` is transcribed and never read, and this client has no
drawn/undrawn state to hang it on, so a character stands in town holding a
claymore. `foss-wow#42`.

**The UI question is answered: this client draws its own interface and does not
run addons.** Reimplementing `FrameXML` faithfully enough for third-party addons
means reproducing a whole Lua/XML widget system before the first health bar
appears. Instead the interface *is* the customisation surface: every position,
size and colour lives in `%APPDATA%\open-wow\ui.toml`, editable by hand or by
dragging frames in-game. egui is the drawing substrate only — frames are painted
from explicit geometry, so `scale` multiplies every dimension and the appearance
is a function of our `Style` alone. See `docs/UI.md`.

**The two halves have met, the viewer drives movement, and it draws the
replicated world moving.** `wow-viewer --realm-host <host> --user <account>
--character <name>` logs in, enters the world, and streams the map the server
chose around the position it reported. W/S walk, A/D turn, **Q/E strafe, Space
jumps, right-drag steers the character while left-drag swings the camera, the
wheel zooms and Num Lock is autorun** — each sent as a real `MSG_MOVE_*` stream,
with the opcode naming the axis that changed and the flags carrying the whole
state. The camera follows behind rather than flying freely. **Left-click
selects; right-click selects and attacks** — hostility is not yet known
(`FactionTemplate.dbc` is untranscribed), so the client rules out only what is
never a fight and lets the server refuse the rest. Altitude follows the terrain — the keys drive the
two horizontal axes and Z is read back out of the height field the ground is
drawn from, so the character walks over hills rather than into them. `LiveWorld` keeps a `world::WorldState` alongside
the connection and folds every drained packet into it, so creatures slide along
their actual path instead of jumping between snapshots or standing wherever they
were at login — turning to face the way they're moving, playing the model's own
walk cycle in motion and its stand cycle at rest, all re-evaluated every frame.

**Known defect: this is true of creatures and not of other players.** A creature
moves by `SMSG_MONSTER_MOVE`, which carries a start, an end and a duration, and
that is what `interpolated_position` was built for. A player moves by relayed
`MSG_MOVE_*`, which carries a position and no path at all, so
`update_movement` stores it and clears any prediction — the player snaps from
packet to packet and, having no duration, reads as `speed: 0.0` and never leaves
the stand cycle. Two symptoms, one cause, and a live report was what surfaced
it.

**Why 3.5's two-client test missed it is the more useful half.** Both clients in
that test were *this* client, which heartbeats every 100ms; a hundred
milliseconds of snap between two nearby points reads as movement. A real client
sends roughly every 500ms, and at that spacing the same bug is unmistakable —
which is exactly how it was reported. Two copies of our own client agreeing is
the weakest form of the two-client rig, and this is the first time that has cost
anything: the rig proves a *format* travels both ways, and proves nothing about
timing that both copies share. See `foss-wow#22`. Verified with two clients, one walking while the
other, running the real viewer, drew it happen; four real bugs in that
drawing path (an animation that never went idle, a whole species animating
because one instance of it moved, entities facing a constant wrong direction,
motion that stuttered once animation ran faster than position updates) were
only found by watching it live — see `docs/ROADMAP.md`'s 3.5 section.
`wow-cli world --enter X --walk 20` remains the CLI-driven equivalent, useful
when no window is available.

On top of that there is now an interface: a player frame and a target frame
drawn from replicated fields, a left click that casts a ray through the cursor
and sends `CMSG_SET_SELECTION` for whatever it hits, and `F1` to drag the whole
layout around and save it. The fields those frames read are confirmed against
the live realm via `wow-cli world --units` — `Testwolf` reads as a rage user
with `0/1000`, not the `0/0` mana a mis-indexed power array would give, and a
second account's player replicates the same way. Watched live too: overlapping
creatures each select deliberately, a bracket of corner ticks marks the
selection out in the world, and left-drag swings the camera around the
character. Two bugs came out of that look and out of nothing else — no
in-world selection marker at all, and a camera whose yaw was written by a drag
and overwritten by the follow code a millisecond later.

4.2 added chat and names on top: `Enter` opens a line to type in (and takes the
keyboard away from movement while it is open), the scrollback colours by kind,
and frames say `Young Wolf` rather than `Creature 299`. Verified across two
clients — `Watcher` on `ACCOUNT34` whispered `Testwolf` on `ACCOUNT33` and it
arrived — plus 50 names resolved from 50 queries with none unanswered. Watched
live, and for the first time in this phase the look found **nothing**: the
typed line went out and came back exactly once, typing did not walk the
character, and a whisper from a player who was never in visibility range
resolved from a bare guid to their name retroactively. That is not extra care;
it is that 4.1's live bugs had been converted into headless checks (a paint
assertion, and received chat logged as well as drawn), and the one bug 4.2 did
have was caught by reading the viewer's own log rather than by looking at it.

## Orientation

- `crates/` — one library per concern: `chunk` (shared chunked container),
  `mpq`, `dbc`, `blp`, `m2`, `wmo`, `adt`, `render`, `auth`, `world`, `ui`
  (the player's interface; depends on neither `world` nor `render`, so it is
  testable without a connection or a GPU)
- `tools/wow-cli` — inspection CLI. **Every format gets a dump command here
  before it is wired into the renderer**, and a `survey` command that parses the
  whole archive set. Those surveys have caught every systematic parser bug so
  far.
- `apps/viewer` — windowed viewer. `--screenshot` renders one frame headless to
  a PNG, which is how render output is checked without a display.
- `docs/` — `ROADMAP.md`, `RENDERING.md`, `PROTOCOL.md`, `UI.md`,
  `REUSE-POLICY.md`, and `formats/*.md` recording what each format actually
  does and where it bit us.

## Local setup

- Source lives on an SMB share (`N:`), which cannot execute binaries. The
  gitignored `.cargo/config.toml` redirects `target-dir` to local disk; without
  it every build fails with `Access is denied (os error 5)`.
- Reference installation: `D:\Games\World of Warcraft 3.3.5a` (verified 12340,
  enUS, 17 archives, 203,949 paths). 1.12.1 and 2.4.3 are also on disk for
  format-evolution comparison.
- `WOW_DATA` supplies `--data` to `wow-cli` and gates the integration tests.
- Test realm: **`wow1.nekos.farm`** (auth 3724, world 8085), realm `NekoCore`
  at `108.174.48.199:8085`, realm id 1. Accounts `TESTER`, `ACCOUNT33` and
  `ACCOUNT34` exist. **Passwords are deliberately not recorded here** — this
  file is committed. Ask the user, and pass the password via `WOW_PASSWORD`
  rather than an argument. A wrong password and a missing account are hard to
  tell apart, so guessing wastes real time.
- Two accounts exist so that **two clients can be online at once**, which is the
  only way to test anything about one player observing another — relayed
  movement, entity replication. A single account cannot prove any of it.
- `ACCOUNT33` also has `Facetest` (human warrior), created deliberately with
  **five different non-zero appearance values** — skin 3, face 5, hairstyle 7,
  hair colour 2, facial hair 4. Every other character here was made with the
  all-zero default, and an all-zero appearance makes any search for it match
  every zero field in the object, which is how two attempts at locating
  `PLAYER_BYTES` settled nothing. Keep it: it is the only character on either
  account that can distinguish a field from its neighbours, or show hair at all.
- `ACCOUNT33` has two characters, `Testwolf` (human warrior) and `Testdruid`
  (night elf druid), created to give `SMSG_CHAR_ENUM` real data to parse. An
  account with no characters exercises none of that packet's field offsets.
  `ACCOUNT34` has `Watcher` (human warrior), deliberately a human so it spawns
  in Northshire within view range of `Testwolf` — two clients in different
  starting zones cannot see each other and prove nothing.

## Rules that matter

1. **Never commit game assets** — not as fixtures, not as test data. Tests
   needing real data read `WOW_DATA` and skip when unset.
2. **No GPL code in the tree.** TrinityCore/MaNGOS may be read to understand a
   field's meaning; implementations are written from public documentation.
3. **WoW-specific formats are implemented in-tree.** Generic plumbing (codecs,
   GPU, windowing, math, crypto primitives) comes from crates.io. The test:
   would this dependency exist if WoW had never been written?
4. **Surveys are the regression net.** `wow-cli verify`, `dbc check`,
   `blp survey`, `m2 survey`, `wmo survey`, `adt survey` each parse everything
   of their kind; a systematic error shows up as one large bucket rather than
   scattered noise.

## How this project finds bugs

Worth reading before debugging anything, because the same shapes keep recurring.

- **A wrong field offset parses perfectly and returns nonsense.** Check
  properties the data must have, not just that it decoded: M2 normals are unit
  vectors, SRP6 rotation keys are unit quaternions, terrain chunks must meet at
  their edges. Each of those caught a real bug that size checks missed.
- **"Could this column mean X" is the wrong question; "is it set *because* of
  X" is the right one.** Finding which of `Spell.dbc`'s 234 columns holds a
  duration by asking which one contains a valid `SpellDuration` id gave a
  99.6% match — on the wrong column. Any column of small integers points
  somewhere inside a 130-row table, so validity is nearly free and proves
  nothing. Comparing the spells whose description says `$d` against those that
  do not immediately separated the real column: non-zero 98.5% of the time
  versus 39.0%. The same reframing found every other column here, and the one
  test that came back flat had been asked the sloppy version of its question
  (every description *mentioning* `$m1`, rather than only those quoting a
  range, which is when a die is actually needed). A property test is only as
  good as the population you run it against.
- **A number nobody can check is worse than a blank.** A wrong field offset
  eventually fails loudly; a wrong *number* on a tooltip never does. So the
  description substituter resolves only the tokens whose columns were
  confirmed against the data and passes everything else through with its `$`
  intact — a visible `$s1` says "not implemented", a fabricated `47` says
  nothing and is believed. Same rule as `describe_cast_failure` naming one
  status code, one layer up.
- **Print the body, not the length, of anything you refuse.** A parser that
  declines an unconfirmed shape is only useful if the shape survives the
  refusal. `SMSG_ATTACKERSTATEUPDATE` arrived four bytes longer than any packet
  seen before, the cursor caught it as trailing bytes -- and the tool logged
  the *length* and dropped the bytes, so the one packet that could have
  answered the question was seen and lost. Two separate tools here had the same
  hole. If a parser's own doc comment says "a capture would settle this", then
  something has to be keeping captures.
- **A reply you cannot get is not the same as a reply you did not earn.**
  `CMSG_ATTACKSWING` could not be read off a capture -- nothing acknowledges an
  opcode, and an outgoing number that is wrong gets read as some *other* valid
  request rather than refused. Sent from out of range and facing the wrong way
  it produced two empty-bodied refusals and no damage, which looks exactly like
  a wrong opcode. The proof was not that a reply came, but that the reply
  *changed when the conditions did*: closing to melee and turning to face
  turned those same refusals into an attack-start and fifteen swings. When
  nothing can confirm a send directly, find the input you can vary and check
  the output varies with it.
- **State that persists needs accounting, not just parsing.** Every parser here
  is memoryless: a bad packet gives one wrong answer and the next is unaffected.
  Replicated world state is not — a dropped update is permanent, a merge that
  overwrites erases fields nothing will resend, a missed removal leaves a ghost.
  None of it errors and all of it compounds. Count every change, tally updates
  naming unknown objects instead of inventing them, and check the books balance
  (`created - removed == held`). Those counters catch replication bugs long
  before the world looks wrong, and none of them assert anything about layout.
- **Assert the parse consumed the whole record.** The corollary to the above,
  and cheaper than any of it. Four separate world-protocol bugs — a packet
  sixteen bytes longer than expected, three missing equipment slots, a
  result-code enum off by one, a position block read as nine floats instead of
  eight — were invisible field by field and obvious the moment a cursor reported
  leftovers. Parse through a cursor and make running out of input *and* having
  input left over both errors.
- **The hard-looking part is rarely the expensive one.** SRP6, the RC4 header
  cipher and the update-field bit-packing all worked close to first time: they
  are precisely specified and fail loudly. Every hour actually lost went to
  ordinary struct layout, where a wrong guess parses perfectly. Budget for the
  boring parts.
- **Check that your check is current.** The first walk was declared a failure
  because the character list still showed the old position — but the character
  list reports the last *saved* position, which lands tens of seconds after a
  disconnect, while `SMSG_LOGIN_VERIFY_WORLD` reports the live one. The movement
  had worked all along. When a change appears not to have taken, confirm the
  thing being read is the thing being written.
- **Give the other end time to act before concluding it ignored you.** A single
  packet sent immediately before disconnecting is often never processed, and the
  result is indistinguishable from having sent the wrong thing. A facing opcode
  was briefly written off as wrong on exactly this evidence; half a second of
  waiting made it work every time.
- **Writing a format is riskier than reading it.** A bad read fails loudly at a
  known offset; a bad write is accepted as some other valid message and shows up
  as wrong behaviour far away. Where a structure travels both ways, define it
  once and round-trip it — two copies of a conditional layout can drift, and the
  outgoing copy has nothing to announce the drift.
- **When a send produces no reply at all, inventory what *did* arrive before
  improving your guess.** Sending chat failed silently three times and looked
  identical every time: packet out, session alive, nothing back. The causes were
  a language an ordinary account may not speak, an enum where `0` is `SYSTEM`
  and `1` is `SAY`, and — twice — our own tooling receiving the reply and
  discarding it. Each round of guessing at the layout was wasted; the moment
  `wow-cli world --stay` printed every opcode seen, decoded or not, the answer
  took one run. "The server never sent it" and "it arrived and we could not read
  it" are the same observation until something separates them, and they want
  opposite investigations.
- **Convert every live-only bug into a check that runs without a window.**
  3.5 and 4.1 each cost a handful of bugs that no test could have caught. 4.2
  cost none, and the difference was not care: 4.1's failures had been turned
  into a headless egui pass asserting a frame painted where the layout put it,
  and into logging received chat as well as drawing it. 4.2's one real bug —
  a chat line stamped with a guid before its name resolved — was then found by
  *reading the viewer's log*, a step earlier than looking. Live testing does not
  stop mattering; each live bug just stops recurring for free.
- **A limit that bounds packets does not bound time.** The login burst drained
  until the stream went quiet *or* 512 packets arrived. Northshire emits a
  monster move fourteen times a second and is never quiet, so the drain ran
  until it had its 512 -- **thirty-seven seconds**, before the client drew a
  single frame. Nothing was wrong with the drain; its contract simply had no
  clock in it. Any "read until N or until idle" loop against a live stream
  wants a wall-clock budget too, and the chunk size then sets how far past that
  budget it can overshoot.
- **Measure the thing, not the thing next to it.** That same delay presented as
  the action bar filling half a minute after login, and the confident diagnosis
  was a slow `Spell.dbc` read blocking the render thread -- with a plausible
  argument attached (two runs agreed to the second, so it must be a fixed cost
  rather than network jitter). It was wrong. The DBC load takes 185ms; the
  spellbook had been sitting at the end of a burst that took 37 seconds to
  finish collecting. One timing log around the suspected culprit settled in one
  run what reasoning had got backwards.
- **Do not transcribe a table you have not verified — especially one that only
  produces text.** A wrong field offset eventually fails loudly; a wrong *name*
  for a status code never does. It confidently misexplains what happened and
  sends the next reader somewhere else. `describe_cast_failure` therefore names
  exactly one reason, the one actually observed against the realm, and returns
  the raw number for everything else. The urge to fill in the whole enum from
  memory is the same urge that produced `CHAT_MSG_SAY = 0x00`.
- **One dispatch table does not save a caller from ignoring what it produces.**
  `WorldState::replicate` is deliberately the only place opcodes are dispatched,
  and that is still right — but chat is *returned* rather than stored, and three
  separate callers quietly dropped it. A two-client test then showed chat never
  being delivered when it had arrived and been thrown away. Centralising the
  producer does not centralise the consumers.
- **Not every failure is a bug.** The world connection dropping after three
  keepalives was the server enforcing a *minimum* ping interval — pinging too
  eagerly is punished harder than not pinging. It surfaced as an unexpected end
  of stream, which is indistinguishable from a desynchronised cipher. Before
  suspecting corruption, ask whether a rate limit or anti-abuse rule was tripped.
- **But derive from the *same* source when two things must agree exactly.**
  The opposite-sounding rule, and both are right about different situations.
  Independence is evidence when you are checking whether something is
  *correct*. It is a liability when two things must *stay* consistent: the
  picking ray is unprojected from the very matrix the scene is drawn with, not
  rebuilt from the camera's angles, because those two agree only until someone
  changes the projection — and a ray that is off by a little lands clicks on
  the creature *beside* the one under the cursor, which reads as the server
  disagreeing about positions rather than as a stale copy of a matrix. Same
  reasoning as defining a both-ways structure once and round-tripping it.
- **Two copies of your own client are not two independent derivations.** The
  two-client rig is this project's strongest shape *for formats*: a structure
  goes out through one client and back in through another, so the write and read
  halves are confirmed via a third party. It proves nothing about behaviour the
  two copies share. 3.5 declared replicated players smooth on exactly that
  evidence, and they were not — both clients heartbeat every 100ms, and a
  hundred milliseconds of snap between nearby points reads as movement. A real
  client sends every ~500ms and the same missing interpolation is obvious. When
  the thing under test is *timing* rather than layout, one of the two ends has
  to be something you did not write.
- **Compare against something derived independently.** The SRP6 tests carry a
  server written from the protocol, not from the client. Agreement between two
  separate derivations is evidence; a thing checked against itself is not.
  The strongest version of this available here is the two-client movement test:
  the structure goes out through one client, through the server, and back in
  through another, so the write and read halves are confirmed against each other
  *via a third party* that had to understand both. Reach for that shape whenever
  a format travels in both directions.
- **Two bugs can share one symptom, and you will fix the innocent one.**
  M2 geometry drawn with the wrong winding culls front faces, which does not
  look like missing geometry -- it looks like a model *facing away from you*,
  because what survives is the interior of its far surface. On that reading a
  half turn was added to entity facing, then the same wrong reasoning was
  propagated to doodads. Neither rotation was ever wrong. What separated them
  was fixing the winding first and then A/B-ing the rotation live, one
  variable at a time, with the person at the window pressing the key. When a
  symptom persists across a fix that should have worked, suspect that it has
  two causes rather than that the fix was too small.
- **And one bug can produce several unrelated-looking reports.** The mirror of
  the rule above, and it costs the same way. A character sinking into the
  ground, a click marker landing off-centre, hills that could not be walked up,
  and another client seeing this one twitch were four separate complaints, none
  of which said "altitude" — and they were one missing feature. The click
  marker in particular reads as a picking-ray bug, because the ray starts at
  the eye and the eye is a fixed offset above a position whose Z was wrong.
  Before opening the second investigation, check whether the first cause
  reaches it.
- **A composite needs a way to be seen as itself.** A dressed character looked
  bare-chested at walking distance and the obvious diagnosis was that the torso
  region was wrong. Dumping the composed 512x512 skin to a PNG showed all ten
  regions correct and the torso wearing a white shirt that simply reads as skin
  at three hundred pixels. The render was right and the *look* at it was wrong,
  which is the inverse of the usual failure here and just as expensive. Anything
  assembled in memory from a dozen files gets a dump command.
- **A trap documented at one call site does not protect the next one.** That
  the server never relays our own movement back — so replicated state holds
  our *login* position forever — is written up at length in
  `live::drawable_entities`, which is the function that **draws** the player.
  It was then walked into immediately by a function that **aims at** the
  player: resolving "face this guid" through replicated state made every
  creature attack the spot the character logged in at, drifting further wrong
  the further they walked, until the player could stand behind a creature
  supposedly fighting them. Same fact, different consumer, and the comment was
  in the wrong place to help. When a fact about the data is surprising enough
  to document, put it on the *data* — an accessor or a type — not on the first
  caller that tripped over it.
- **A packet is a statement made once; a field is a statement that stays
  true.** Creature facing was first driven off `SMSG_MONSTER_MOVE`'s facing
  block, which the server sends only when it decides a creature has turned.
  The result was a wolf that turned *only when the player moved*, because that
  is what prompts the server to re-issue one. `UNIT_FIELD_TARGET` says who a
  unit is fighting for as long as it is fighting them, so deriving the heading
  from it tracks continuously and for free. When behaviour should be
  continuous, prefer the replicated field over the event that last changed it.
- **A rate limit and a lag are different failure modes, and only one of them
  is bounded.** Easing creature turns at a fixed maximum rate looks right in
  every single frame, and is fine while the target's angular speed stays under
  the cap. Angular speed is `v / r`, so a player circling at melee range
  exceeds any cap chosen to look unhurried — and past that point the error does
  not settle at "somewhat behind", it grows without limit until the creature
  faces nowhere near its victim. Closing a *fraction* of the remaining error
  instead bounds the lag at `omega * tau` for any `omega` at all. Whenever a
  smoothing constant is a maximum speed, ask what happens when the input
  exceeds it.
- **A fallback can invalidate the rule that was written beside it.** Combat
  animations fall back to plain `Stand` on a model with no attack or ready
  cycle — which a wolf genuinely lacks. That fallback then broke the
  *clamping* rule sitting next to it: "plays once, so hold the last frame" was
  keyed on the state that asked, and a Stand frozen at its final frame is a
  statue. The same fallback broke it the other way too — `Dead` resolves to
  the *fall* on those models, which must hold rather than loop, while carrying
  no start time to clamp against. Holding had to become a property of the
  animation that resolved rather than of the state that requested it. When a
  fallback is added, re-ask every question that was answered in terms of the
  thing being fallen back from.
- **Copying a mechanism is the cheapest way to audit it.** Right-click needed
  the same press/release distance test the left button had used since 4.1, so
  it was written by mirroring it — and mirroring it surfaced a bug four
  milestones old. The left button cleared `last_cursor` on release, and the
  *next* press reads its own start position out of that same field, so a
  second click at the same pixel was silently discarded. It survived because a
  selection is not a gesture anyone repeats; right-click-to-attack is, and
  exactly when it appears not to have worked. Reusing a mechanism in a second
  place asks questions of it that the first place never did.
- **A grep that finds the field names is not a grep that finds the
  structure.** Reading AzerothCore for the jump block's field order turned up
  `sinAngle, cosAngle, xyspeed, zspeed` in `MovementHandler.cpp` — a different
  order from this project's `Falling`, and it looked exactly like a bug worth
  fixing. It was a different packet. The canonical `MovementInfo` codec in
  `WorldSession.cpp` reads `zspeed, sinAngle, cosAngle, xyspeed`, which is what
  we already had. "Fixing" the correct one would have been silent, and the
  source would have been blamed for it. When source makes a hypothesis cheap,
  confirm you are reading the *definition* and not one of its users.
- **A rule can be right and still exclude the one thing you need — and then
  the test has to assert both halves.** 4.3 established that a spell earns a
  bar slot by belonging to the character's own skill line, because every
  internal effect (`Opening`, `Duel`, `Honorless Target`) sits on
  `SkillLineAbility`'s generic line 183 with a class mask of zero. `Auto
  Attack` sits on line 183 with a class mask of zero. So the mechanism that
  correctly keeps the junk off a bar necessarily hid the one ability every
  character uses, and the rule was not wrong — merely complete. Two fixes
  present themselves and only one is right: widening the rule to admit line
  183 readmits all the junk, where naming the single spell admits exactly what
  was checked. The trap is in the *test*: asserting only that auto-attack is
  admitted passes just as well under the wrong fix, so the check has to assert
  the junk beside it is still refused. Whenever an exception is carved into a
  filter, test the exception **and** the thing it is indistinguishable from.
- **A column can be named correctly and still not mean what its name says
  here.** Every held item in the game -- main-hand swords included -- stores
  its geometry in `ItemDisplayInfo.model_left`, and `model_right` is empty.
  The obvious conclusion is that the two columns are swapped, and it is wrong:
  shoulders fill both and put `LShoulder_...` in one and `RShoulder_...` in
  the other, which proves the names. The pair is really "first model, second
  model", and only a genuinely *paired* item uses both, so a single-model item
  sits in the first column whichever hand it belongs in. Reading the column as
  the hand would have put every weapon in the game in the wrong one, silently.
  When a column's name suggests an answer, find the rows where the name is
  unambiguous -- the pairs, the extremes -- and let those define it.
- **When every measurement says it is right, stop measuring and move.** A
  weapon that would not appear on screen produced four clean diagnostics in a
  row: the item resolved, the group was built, the transform put it exactly at
  the hand, the model rendered fine alone. Nothing was wrong. The camera sits
  behind the character and a blade held forward at hip height is entirely
  behind its owner from there. One render from the side settled it. The
  sibling of "a composite needs a way to be seen as itself", and the tell is
  the *pattern*: diagnostics that keep coming back correct are evidence about
  the observer, not the code.
- **Validity is nearly free; *variation* is the discriminator.** Two update
  fields both resolved 100% to real `GameObjectDisplayInfo` rows, because the
  table is 39% dense and any small integer lands in it. One was the constant 33
  -- the type mask -- and would have drawn thirty-two identical powder kegs;
  the other took seven values that came out as inn benches in the abbey the
  player was standing in. When a candidate column and a control both look
  valid, ask whether the candidate *varies the way the thing it names varies*.
- **Listing a directory and reading a path are different questions.** An MPQ
  resolves by hash, so a file absent from `(listfile)` still reads perfectly.
  A coverage check for the baked NPC textures built on `wow-cli ls` concluded
  0.1% of them shipped and would have sunk the approach; resolving forty random
  names by path got forty hits. When a cheap check says a whole feature is
  impossible, confirm it answered the question you asked.
- **An absent update field is a zero, not an unknown.** An object-create block
  carries only non-zero values, so a player with the default appearance has no
  `PLAYER_BYTES` field at all. Treating absence as "not known" left exactly the
  plainest-looking players white -- the bug the field had just been added to
  fix. The rule generalises: for a sparse field set, missing and default are the
  same statement, and only a dropped *object* means unknown.
- **When geometry is missing rather than wrong, suspect culling before data.**
  WMO winds counter-clockwise, M2 and terrain clockwise. Guessing from a
  neighbouring format culled a roof and looked like a hole in the mesh.
- **Geometry drawn at zero size looks exactly like geometry never drawn.**
  This one recurred, in a second place, years of commits later. A bone palette
  is a fresh GPU buffer, and a fresh GPU buffer is zeroed, and a zero matrix
  multiplies every vertex to the origin -- so a palette created and never posed
  collapses its model to a point in total silence. `--screenshot` placed every
  replicated creature and never called `update_animations`, so a headless
  render of a zone with ninety-five creatures in it came back as empty grass,
  and had done since the feature was written. Nobody noticed because 3.5 was
  verified by watching a *window*, where the frame loop does pose them. The
  buffer now initialises to identity, so the same mistake draws a bind pose --
  visibly wrong instead of invisibly absent. **Anything that must be written
  before it is read should start as something you can see.** A
  bone index past the end of the palette reads zero on the GPU, collapsing the
  model to the origin with no error anywhere. Creatures were invisible while
  doodads rendered, and the obvious reading — that the entities were never
  placed — sent the search to the protocol instead of the renderer. When
  something is missing, confirm whether it was *submitted* before asking whether
  it was produced.
- **Comparing two candidates tells you which is nicer, not which is right.**
  The ADT placement offset shipped at `-90`, was "fixed" to `+90` because a
  render of Northshire Abbey looked better that way, and both were 90 degrees
  wrong -- every fence in Elwynn lay across its own line the whole time. A
  building has four sides and every rotation shows a door to somebody, so the
  test could never fail. What settled it was measuring something that could
  not move: fence *runs* give a direction from positions alone with no
  rotation involved, and the lamp pillars beside the abbey steps are doodads
  whose world positions are fixed however the building is turned. **A movable
  thing checked against another movable thing proves nothing.** And when a
  user says a second thing is still wrong, that is data about the *first* fix.
- **A value with nothing to compare it against is not verified by looking at
  it.** Entity facing was applied raw for four milestones under a comment
  claiming an M2's forward is +X, and every creature in the world was turned
  exactly backwards the whole time. Watching it live could not catch it: the
  only heading this client *knows* is the player's own, and the player's body
  was not drawn, while a creature's heading comes from the server with nothing
  to check it against. It fell out the moment the player appeared on screen --
  turn the character to a heading the server confirms, put the camera at the
  matching yaw, and whether you see a face or a back is no longer a matter of
  opinion. Before trusting a value because it "looks right", ask what it is
  being compared *to*.
- **Some rules can only be found by looking.** Geoset selection -- which of a
  character model's seventeen haircuts and six beards to draw -- took four
  attempts, and each wrong one was a *reasonable* reading of the same table.
  Drawing everything gave every haircut at once; drawing only what the
  character's own numbers name took the forearms, hands and legs off with the
  phantom cloak, because variant one of an equipment group is the bare body
  part. No amount of staring at `CharHairGeosets` distinguishes those. One
  screenshot each did. When a rule is about what a *model file* contains rather
  than what a table says, render it.
- **An odd-looking render is often the camera.** A gnoll looked scrambled and a
  building looked misplaced; both were framing, not geometry. Render canonical
  angles before doubting the parser.

## Traps already hit

- **Never rewrite a file with a script that can throw mid-write.** A Python
  `write_text` containing a character the console codec could not encode
  truncated `docs/ROADMAP.md` to zero bytes. Prefer the editing tools; if a
  script must write, write UTF-8 explicitly.
- `wgpu`/`egui`/`egui-wgpu`/`egui-winit` versions are coupled, and the `windows`
  crate needs a pin to build the DX12 backend at all — see `docs/RENDERING.md`
  before touching any of them.
- Windows refuses to execute a test binary whose filename looks like an
  installer, so integration tests are `tests/real_data.rs`, never
  `real_install.rs`.
- Clap eats a bare negative number as a flag: pass `--pitch=-20`, not
  `--pitch -20`.

## Conventions

- Errors are typed per crate with `thiserror`; `anyhow` only in `tools/` and
  `apps/`.
- Comments explain *why*, especially where the format is counterintuitive —
  those are the notes that stop a bug being reintroduced.
- Byte-level parsing gets a unit test with a known-good constant wherever one
  exists (e.g. the MPQ crypt table keys).
- Commit messages explain the reasoning and record dead ends, so a later session
  does not repeat them. `git log` is part of the documentation.
- Every milestone ends with a clean `cargo build --release` (zero warnings) and
  a full `cargo test --release` run.
