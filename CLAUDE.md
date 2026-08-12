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
| Protocol | **3.1–3.5 done**, all confirmed against a live realm including one client watching another move. Replicated creatures and players slide along their actual path, turn to face it, and play the model's own walk/stand cycles |
| Interface | **4.1 and 4.2 done.** Native, fully customisable, no addons — see the decision below. Player and target unit frames, click-to-target with an in-world bracket, a chat window you can type in, real names, `F1` to rearrange, saved to `ui.toml` |
| Game | **4.3 done**: spellbook, three action bars with real icons, keys `1`-`=` with Shift/Ctrl, click-to-cast, hover tooltips reading real numbers (82% of `Spell.dbc`'s description templates resolve), a cooldown sweep, and a cast bar off `SMSG_SPELL_START`/`SMSG_SPELL_GO`. **4.4 melee done**: swing at a target and be swung at, a named combat log (`You hit Kobold Vermin for 6. Killing blow.`), and a dead unit dimmed in the frames. Spell damage, threat and the corpse flow remain. Inventory and quests follow |

Roughly 57% of the way to something a person could test by playing. See
`docs/ROADMAP.md` for the milestone ladder and what is deliberately deferred.

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
chose around the position it reported. Holding W/S walks the character forward
or backward and A/D turns it, each sent as a real `MSG_MOVE_*` stream
(`MoveStartForward`/`MoveHeartbeat`/`MoveStop`), and the camera follows behind
rather than flying freely. `LiveWorld` keeps a `world::WorldState` alongside
the connection and folds every drained packet into it, so creatures and other
players slide along their actual path instead of jumping between snapshots or
standing wherever they were at login — turning to face the way they're moving,
playing the model's own walk cycle in motion and its stand cycle at rest, all
re-evaluated every frame. Verified with two clients, one walking while the
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
- **Compare against something derived independently.** The SRP6 tests carry a
  server written from the protocol, not from the client. Agreement between two
  separate derivations is evidence; a thing checked against itself is not.
  The strongest version of this available here is the two-client movement test:
  the structure goes out through one client, through the server, and back in
  through another, so the write and read halves are confirmed against each other
  *via a third party* that had to understand both. Reach for that shape whenever
  a format travels in both directions.
- **When geometry is missing rather than wrong, suspect culling before data.**
  WMO winds counter-clockwise, M2 and terrain clockwise. Guessing from a
  neighbouring format culled a roof and looked like a hole in the mesh.
- **Geometry drawn at zero size looks exactly like geometry never drawn.** A
  bone index past the end of the palette reads zero on the GPU, collapsing the
  model to the origin with no error anywhere. Creatures were invisible while
  doodads rendered, and the obvious reading — that the entities were never
  placed — sent the search to the protocol instead of the renderer. When
  something is missing, confirm whether it was *submitted* before asking whether
  it was produced.
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
