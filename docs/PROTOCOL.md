# Protocol

Notes on `crates/auth`, and later the world protocol. Written from public
documentation; no GPL server code is read into this tree.

## Logon server

A short, strictly ordered conversation on TCP **3724**: challenge, proof, realm
list. Everything is little-endian, and nothing is length-prefixed at the top
level, so a reader must know each message's shape to know how much to consume.

Four-character tags — platform, OS, locale — are stored **reversed**, because
they are written as little-endian integers rather than as strings. `enUS`
appears on the wire as `SUne`.

## SRP6

SRP proves knowledge of a password without sending it, and agrees a session key
in the process. That key is not incidental: the world server keys its header
cipher with it, so logging in is a prerequisite for anything else.

Three details this implementation had to get right:

- **Credentials are upper-cased before hashing.** The server stored its verifier
  from the upper-cased form, so a lower-case attempt derives a different `x` and
  is rejected exactly as a wrong password would be.
- **Integers are fixed-width little-endian.** A value that happens to have a
  zero top byte must still fill its field, or every hash computed over it
  changes.
- **The session key is interleaved, and leading zeros are skipped in pairs.**
  `S` is a modular result, so it is shorter than 32 bytes roughly one login in
  256. Skipping an odd number of bytes swaps which half each byte belongs to,
  which fails only on those logins — the worst possible failure rate to debug.

### Testing it without a server

`srp6.rs` carries a **server side written from the protocol definition**, not
derived from the client, and the tests assert that the two independently reach
the same session key and that the server accepts the client's proof. Agreement
between two separately derived implementations is evidence; a client checked
against itself is not.

`client.rs` goes further and runs the real client against a mock server over a
real socket, which is what validates the wire encoding rather than just the
maths.

## Distinguishing "no such account" from "wrong password"

The server returns the same refusal code for both, and the raw code alone is
therefore ambiguous. The *stage* disambiguates it: a refusal at the challenge
means the account was not found, because the server has not seen a password yet
and cannot be rejecting one. A refusal at the proof means the account exists —
its salt arrived in the challenge — and the proof did not match.

Reporting one message for both wasted real debugging time here: a wrong password
looked like a missing account, which in turn looked like the server being down.

## Verified against a live realm

Both paths are confirmed against a running 3.3.5a server:

```console
$ wow-cli auth <host> --user tester --password wrong
Error: account exists but the password was rejected

$ wow-cli auth <host> --user nosuchname --password x
Error: no such account on this realm
```

The challenge parses cleanly from the real server — 119 bytes carrying `B`, the
generator, the prime, the salt and the security flags — which is what confirms
the request is well-formed, since a malformed one would not be answered at all.

A full login now completes end to end, which is what finally proved the SRP6
maths rather than merely the packet shapes:

```console
$ wow-cli auth wow1.nekos.farm --user account33
authenticated. session key is 40 bytes, fingerprint dceb..334f

1 realm(s):
  NekoCore    108.174.48.199:8085    0 characters, population 0.00
```

# World server

`crates/world`. Everything below is confirmed against the same live realm.

## The header cipher

Only packet *headers* are encrypted — four to six bytes of size and opcode —
and bodies travel in clear. That is enough: a reader who cannot find the next
length cannot find the next packet.

Each direction is RC4, keyed with `HMAC-SHA1(seed, session_key)`. The seed is
the HMAC *key* and the session key the *message*, which reads backwards and is
worth stating explicitly. Two fixed 16-byte seeds, one per direction, then a
kilobyte of keystream discarded to skip RC4's biased prefix.

Three properties make this the hardest thing in the protocol so far:

- **RC4 has no integrity check.** A wrong key produces no error, just a
  plausible header with an absurd length. There is nothing to check *at the
  point of the mistake*.
- **The two sides share a position, not just a key.** Decrypting a byte twice,
  or skipping one, desynchronises permanently and the failure surfaces far from
  its cause. The reader therefore uses `read_exact` straight onto the socket:
  a buffered reader that pulled ahead and decrypted speculatively could not
  un-decrypt.
- **The header length is not known until its first byte is decrypted.** Over
  0x7FFF the size grows a third byte, flagged in the top bit of the first, so
  the header arrives in two reads.

The size limit in `protocol.rs` is often mistaken for a key check and is not
one. A wrong key randomises the size; half the time the flag bit lands set, the
size is read from three bytes and the limit rejects it, and the other half it
is read from two, cannot exceed 0x7FFF, and passes. The limit bounds the damage;
what actually detects a mis-keyed cipher is that the expected opcode never
arrives.

## The handshake

```text
server -> SMSG_AUTH_CHALLENGE   plaintext header, carries the seed
client -> CMSG_AUTH_SESSION     plaintext header, proves the session key
          ...both sides start the cipher...
server -> SMSG_AUTH_RESPONSE    encrypted from here on
```

The cipher starts *between* the second and third messages, and that seam is the
whole difficulty. Encrypting the session header, or failing to encrypt the one
after it, desynchronises immediately.

The proof is `SHA1(account | 0 | client_seed | server_seed | session_key)` with
no separators or length prefixes, so every field's width has to be exact. Both
seeds are held as opaque 4-byte arrays rather than integers: they are only ever
echoed into a hash, so keeping them as bytes makes it impossible for the wire
order and the hash order to disagree.

The client also sends an addon manifest — a zlib stream prefixed by its
*uncompressed* length. Confusing the two lengths makes the server size its
buffer wrongly and silently drop the block.

## What the real server corrected

Three errors survived a passing unit-test suite and were caught only by the
live server. All three share a shape: the data parsed perfectly and was wrong.

- **`SMSG_AUTH_CHALLENGE` is 40 bytes, not 24.** It carries *two* 16-byte random
  numbers, not one. 3.3.5a ignores both — they only acquire a use in Cataclysm.
- **A character entry has 23 equipment slots, not 20.** Nineteen worn slots plus
  four bag slots. At twenty slots every individual field of a real character
  still parsed; the only evidence was 27 bytes left over, which is three slots
  at nine bytes each.
- **The character result codes were one too low.** They are positions in one
  long shared enum that also covers the logon results, the realm list and
  account creation, so an extra entry anywhere earlier shifts everything after.
  A successful creation reported "server error" while the character appeared on
  the realm regardless.

The first and second were caught by the same mechanism: every body parser
consumes its packet through a cursor and calls `finish`, which fails if
anything is left over. Neither error was detectable field by field.

`CHAR_CREATE_SUCCESS` (0x2F), `CHAR_DELETE_SUCCESS` (0x47) and the death-knight
level requirement (0x3B) are all confirmed by round trip against the server,
which is what fixes the offset of that whole region of the enum.

## Verified end to end

```console
$ wow-cli world wow1.nekos.farm --user account33
session accepted, header cipher running (expansion 2)

2 character(s):
  Testwolf   level 1  Human Warrior      map 0  at -8950.0, -132.5, 83.5
  Testdruid  level 1  Night Elf Druid    map 1  at 10311.3, 832.5, 1326.4
```

Those coordinates are Northshire Abbey and Teldrassil respectively — the two
races' actual starting points, which is an independent check that the floats
landed on the right offsets rather than merely decoding to finite numbers.

`--create` and `--delete` exist for the same reason: an account with no
characters proves the handshake but exercises none of the character list's
field offsets, and the slot-count error above is exactly what that blind spot
hides.

# Entering the world

`CMSG_PLAYER_LOGIN` carries only a guid. The reply is not a packet but a burst
of fifty-odd: action bars, spell lists, faction standings, the motd, and the
object updates that describe everything in view. Nothing marks its end, so the
only signal that the initial state is complete is the stream going quiet.

## Object updates

`SMSG_UPDATE_OBJECT` is the packet the whole game world arrives through. It is
the least forgiving thing in the protocol, for one structural reason:

**Nothing in it is length-prefixed and every part is conditional on a flag read
a moment earlier.** A movement block's size depends on its movement flags; a
values block's size depends on a bitmask; the block count sits at the front.
There is no way to skip a part that is not understood, because finding where it
ends *is* the act of understanding it. One misread bit and the rest of the
packet is garbage — not detectably so, just quietly wrong.

Three sub-formats carry most of the difficulty:

- **Packed guids.** A mask byte says which of eight bytes are non-zero; only
  those follow. Byte *positions* must be preserved, not compacted.
- **Update masks.** A count, that many 32-bit words of bitmask, then one value
  per set bit in ascending index order. Sparse by nature — a player has over a
  thousand fields and a typical update sets a handful.
- **Movement blocks.** Nested conditionals. Swimming or flying inserts a pitch
  float that nothing else announces; falling adds four more; a spline appends a
  variable-length path.

`SMSG_COMPRESSED_UPDATE_OBJECT` is the same payload behind zlib, with the
*uncompressed* length in front — the same convention as the addon manifest.

## What the real server corrected, again

One packet in forty-nine failed to parse, reporting an impossible update type.
The cause was four bytes: the `UPDATEFLAG_POSITION` layout writes the position
twice, absolute then transport-relative, but the orientation **once**, between
the second copy and a trailing corpse-facing float. Eight floats, not nine.
Reading it as two complete four-float positions overran by one field and
desynchronised the remainder of the packet.

Same shape as every previous protocol bug here: every individual field parsed,
and only the cursor's end-of-packet assertion noticed.

## The keepalive interval is not a free choice

`CMSG_PING` every thirty seconds. Pinging *faster* is punished: the stock server
counts any ping under about 27 seconds after the previous one as "overspeed" and
disconnects after a couple of them. A client that pings eagerly to be safe is
dropped sooner than one that never pings at all.

Found the hard way. At five-second pings the realm closed the connection after
the third, and it surfaced as an unexpected end of stream — indistinguishable
from a desynchronised header cipher, which is exactly where the debugging went
first.

`SMSG_TIME_SYNC_REQ` must also be answered, and answered from wherever the
client happens to be in its read loop, so it is handled centrally rather than by
whichever call site is waiting. Note the ordering this produces: the client
sends `CMSG_CHAR_ENUM` before it has *read* the burst containing the time-sync
request, so its answer necessarily arrives second. It cannot answer a packet it
has not read yet.

## Verified end to end

```console
$ wow-cli world wow1.nekos.farm --user account33 --enter Testwolf --stay 100
  in world on map 0 at -8950.0, -132.5, 83.5 facing 0.00 rad

object updates: 50 parsed, 0 failed
  135 blocks: 130 created, 4 left view
    game object x36   item x7   player x1   unit x86
  own player object (guid 0x32):
    level 1   health 60/60   faction 1   display id 49
    guid field 0x32 (matches)
    118 fields set

holding the connection for 100s
  still connected: 510 packets seen, 3 keepalives answered
```

The checks that matter are the independent ones. Health 60 and display id 49
are what a level 1 human warrior has; the night elf druid on the other character
reads 54 and 55. The guid appears twice — once in the block header, once as
field 0 — written by different server code, and they agree. None of that is
checkable against the parser itself.

# Movement

`MSG_MOVE_*` — `MSG_`, not `CMSG_`, because the same opcode travels in both
directions: the client reporting its own movement and the server relaying
someone else's.

This is the first thing the client **sends** whose layout it also has to get
right, and that changes the failure mode completely. Everything before it was
read-only, where a wrong guess produced a parse error at a known offset. A
malformed movement packet produces no error at all — the server reads it as some
*other* valid movement, and the first sign is a character standing somewhere
unexpected.

So `MovementInfo` is defined once in `movement.rs` and both directions go
through it. Object updates read it; the client's own movement writes it. A field
that is wrong is then wrong symmetrically, which a round trip catches; two
copies could drift, and the outgoing copy has nothing to announce the drift.

The write side emits its optional parts from the **flags**, never from whether
the corresponding field is populated. The reader on the other side has only the
flags to go on, so a writer consulting anything else can produce a packet
nothing can parse back.

## The mover guid

Each packet begins with a packed guid saying *which* thing moved, before the
movement state. It is easy to leave out — the client is obviously talking about
itself — but WotLK added it so a player controlling a vehicle or a
mind-controlled creature can say so. Omitting it does not fail cleanly: the
server reads the first bytes of the movement flags as a guid and everything
after shifts.

## Movement is a stream, not a request

Start, a heartbeat roughly every 100 ms, then stop. Sending only the endpoints
is the obvious shortcut and the wrong one: the server integrates position
against elapsed time, and one jump across the whole distance is the exact shape
of a speed hack. Stopping matters too — a character left in the `FORWARD` state
keeps moving in the server's simulation after the client goes quiet.

## Movement is two axes, and the opcode names only the one that changed

Walking and strafing are independent: a character can begin sidestepping
without stopping running, and does so constantly in play. The wire says this
with a *pair* of start/stop opcode sets, one per axis:

| axis | start | stop |
|---|---|---|
| longitudinal | `MSG_MOVE_START_FORWARD` `0x0B5`, `MSG_MOVE_START_BACKWARD` `0x0B6` | `MSG_MOVE_STOP` `0x0B7` |
| lateral | `MSG_MOVE_START_STRAFE_LEFT` `0x0B8`, `MSG_MOVE_START_STRAFE_RIGHT` `0x0B9` | `MSG_MOVE_STOP_STRAFE` `0x0BA` |

**The opcode names the transition; the flags carry the whole state.** Beginning
to strafe while already running forward sends `MSG_MOVE_START_STRAFE_LEFT`
with *both* `MOVEMENTFLAG_FORWARD` (`0x1`) and `MOVEMENTFLAG_STRAFE_LEFT`
(`0x4`) set. A client that sent only the bit matching its opcode would be
telling the server it had stopped running the moment it started strafing --
and since nothing acknowledges movement, the only symptom would be a character
that drifts.

`MSG_MOVE_STOP` likewise ends only its own axis. A character that stops running
while still holding a strafe key keeps strafing, and the flags say so.

## Jumping is a pair of statements

`MSG_MOVE_JUMP` `0x0BB` says a character left the ground and carries the
falling block -- `zspeed`, `sinAngle`, `cosAngle`, `xyspeed`, in that order,
present only while `MOVEMENTFLAG_FALLING` (`0x1000`) is set.
`MSG_MOVE_FALL_LAND` `0x0C9` says it arrived, with the flag cleared and
`fall_time` carrying how long the fall lasted -- which is what fall damage is
computed from.

**The landing is not optional.** The server believes a client that said it was
falling until it says otherwise, so a jump that never lands leaves the
character permanently airborne in the server's view, silently.

The server does not simulate the arc. It is told the take-off velocity and the
landing and believes the client in between, which is why the client carries its
own gravity: `19.29110527038574` units per second squared, matching the
server's own constant so a reported `fall_time` agrees with the height fallen.
The *take-off* velocity is the client's choice and appears in no server table;
this project uses `7.9558` and says plainly that it was chosen rather than
measured. One capture of a real client's `MSG_MOVE_JUMP` would settle it.

### Confirmed by relay, which is the only way an outgoing opcode can be

Nothing acknowledges any of these, so the check is the two-client rig: one
session moves, a second session on a different account watches what the server
relays. Against the local realm, `wow-cli world --enter Testwolf --jump`
produced, in the watcher's opcode census:

```text
MSG_MOVE_* relayed (0x00bb)      x1
MSG_MOVE_* relayed (0x00c9)      x1
```

and `--strafe right --walk 15`:

```text
MSG_MOVE_* relayed (0x00b9)      x1
MSG_MOVE_* relayed (0x00ba)      x1
```

Exactly one of each, in order. That is a stronger statement than "the session
survived": a body the server could not parse as a jump would not have been
relayed *as a jump* to somebody else. The write half is confirmed through a
third party that had to understand both.

The strafe has a second, independent confirmation that does not involve
opcodes at all. Facing 4.61 rad, `--strafe left --walk 20` moved the server's
own position from `-8939.3, -197.5` to `-8919.4, -199.5` -- **+19.9 in x and
-2.0 in y, with the orientation unchanged**. Sideways without turning is
something a forward walk cannot produce at any heading, so the direction is
right and not merely plausible.

### Six more confirmed, nine that cannot be reached yet

`world::opcode::server::MOVE_RELAYED` lists 24 opcodes; the above accounts for
nine. `foss-wow#37` went after the other fifteen with the same two-client rig,
now against `Testwolf` and a revived `Watcher` teleported to stand beside him
on the local realm (`.go xyz`), watching with `wow-cli world --enter Watcher
--stay 240 --capture`.

Six had never been sent by this client at all -- nothing before this ticket
could turn on the spot or toggle run/walk, since `world::motion::Motion` only
ever modelled the two translation axes. `client.rs` gained `turn_in_place` and
`set_run_mode`, the same start/heartbeat/stop and single-packet shapes
`travel`/`set_facing` already used, reachable as `wow-cli world --turn
<left|right>` and `--run-mode <run|walk>`. One capture, both new commands plus
`--jump`, gave `wow-cli moves` this:

```text
opcode   packets movement  extra refused handled  movers
0x00bb       1       1      0      0     yes  1
0x00bc       1       1      0      0     yes  1
0x00bd       1       1      0      0     yes  1
0x00be       2       2      0      0     yes  1
0x00c2       1       1      0      0     yes  1
0x00c3       1       1      0      0     yes  1
0x00c9       1       1      0      0     yes  1
```

`0x00bb` `MSG_MOVE_JUMP`, `0x00bc`/`0x00bd`/`0x00be` `START_TURN_LEFT`/
`RIGHT`/`STOP_TURN`, `0x00c2`/`0x00c3` `SET_RUN_MODE`/`WALK_MODE`, and
`0x00c9` `FALL_LAND` along for the ride from `--jump` -- every one 100%
movement, 0 bytes left over, exactly one mover, the same bar the original five
cleared.

**The remaining nine were not driven, and the reason is a fact about this
client rather than about the opcodes.** `START_PITCH_UP`, `START_PITCH_DOWN`,
`STOP_PITCH` and `SET_PITCH` only arise while swimming or flying; `START_SWIM`
and `STOP_SWIM` need water; `START_ASCEND`, `STOP_ASCEND` and `START_DESCEND`
need a flying mount. This client has no swim state, no mount, and no pitch
field on any packet it sends -- so reaching them is new capability, not
another capture. A level-one human warrior standing on dry land cannot
produce any of the nine, and `MOVE_RELAYED`'s own doc comment says so
plainly rather than leaving the gap to look like an oversight.

## Nothing acknowledges movement

There is no reply. A rejected move produces no error; the server simply keeps
its own idea of where the character is. Confirmation has to be obtained by
asking again later, and there are two ways, which disagree:

- **`SMSG_LOGIN_VERIFY_WORLD` on re-entry** reports the live position
  immediately.
- **The character list** reports the position last written to the database,
  which happens when the previous session's logout-save lands — tens of seconds
  after an abrupt disconnect.

Checking the character list immediately reads the save *before* last and looks
exactly like movement having been ignored. That cost a debugging detour here:
the first walk was declared a failure on the strength of a character list that
was simply not caught up yet. Waiting 35 seconds showed the correct position.

## Verified against a live realm

Four legs of 20 units on headings 0°, 90°, 180° and 270°, each run re-entering
the world before walking:

```text
leg   0:  in world at -9000.0, -122.5  ->  -8980.0, -122.5
leg  90:  in world at -8980.0, -122.5  ->  -8980.0, -102.5
leg 180:  in world at -8980.0, -102.5  ->  -9000.0, -102.5
leg 270:  in world at -9000.0, -102.5  ->  -9000.0, -122.5
```

Every leg's starting position, read from the server, is the previous leg's
endpoint, and the square closes on the point it started from. The facing carries
over too. Chaining the legs is what makes this evidence: any single move could be
a coincidence of a stale read, but four in sequence returning exactly to the
origin could not.

## Turning on the spot

Orientation normally reaches the server as a side effect of the position in a
movement packet, so a character that turns without walking keeps its old facing
for everyone else. `MSG_MOVE_SET_FACING` (0x00DA) reports it on its own;
confirmed by turning to 90° and 270° and reading the value back on re-entry.

## A packet sent just before disconnecting can be lost

Worth its own note, because it produces a false negative that looks exactly like
a wrong opcode. The first `MSG_MOVE_SET_FACING` test sent one packet and exited
immediately; the facing did not change, and the obvious reading was that 0x00DA
was wrong. It was not — the server had not processed the packet by the time the
socket closed. Holding the connection open for half a second afterwards makes it
take, every time.

Walking never showed this because a walk takes seconds and its last heartbeat is
long since processed. Anything that sends a single packet and leaves needs to
wait, and the CLI's `--face` now drains briefly before returning for exactly
this reason.

## Seen moving by another client

Two accounts, two characters standing about thirty units apart in Northshire:
`Testwolf` on one, `Watcher` on the other. `Watcher` held its connection open
while `Testwolf` walked 40 units.

```text
Testwolf (walking):   -8949.9, -127.9  ->  -8912.3, -114.2
Watcher  (observing): guid 0x32, 60 packets,
                      -8949.9, -127.9  ->  -8912.3, -114.2  (40.0 units)
```

The observer's path, decoded from relayed `MSG_MOVE_*` packets, matches the
walker's own record exactly.

This is the most valuable test in the protocol work so far, and not because of
the milestone it closed. The movement structure went **out** through one client,
through the server, and **back in** through another. The write half and the read
half of `MovementInfo` were therefore confirmed *against each other through a
third party* — the server had to understand what was written in order to relay
something the reader could understand. A shared bug in both halves would have to
have been a bug the server also shared, which is a far narrower target than a
self-consistent mistake.

It also exercises the inbound relay path with real data for the first time. Up
to this point it was written, wired, and had never received a packet.

# Replication

The server never sends the world; it sends *changes* to it. An object arrives
once as a create block carrying everything, and after that only what altered: a
`Values` block with three fields, a movement packet with a position, a guid in
an out-of-range list. `state.rs` folds that stream into a live view.

This is the first place in the project where a mistake **survives**. Every
parser before it was memoryless — a bad packet produced a bad answer once, and
the next packet was unaffected. Here a dropped update is permanent, a merge that
overwrites instead of merging erases fields nothing will resend, and a missed
removal leaves a ghost standing where nothing is. None of it errors, and all of
it compounds.

So the defences are about **accounting** rather than parsing. Every change is
counted; updates naming an unknown guid are tallied as orphans rather than
fabricating an entity; re-creations are counted separately from creations. A
replication bug shows up as a number that does not add up long before it shows
up as a wrong world. The invariant to watch is `created - removed == objects`.

Two rules that are easy to get backwards:

- **A `Values` block is a merge, not a replacement.** It carries only what
  changed. Applying it as a replacement leaves a creature that took damage with
  no level, faction or model, and the loss looks like the *create* block having
  been mis-parsed rather than discarded afterwards.
- **A monster move places the creature at the path's start, not its end.** The
  packet describes travel about to happen over its stated duration. Jumping to
  the destination on arrival makes every creature in the zone teleport.

## `SMSG_MONSTER_MOVE`

By a wide margin the most common packet in a populated zone — a single login
burst in Northshire carried nearly four hundred. Two things in it bite:

- **A stop ends the packet early.** Reading past it consumes the next packet.
- **The path has two encodings.** Catmull-rom and flying paths carry every point
  in full; everything else carries the destination in full and the intermediate
  points as offsets from the midpoint, packed three to a word. Picking the wrong
  one desynchronises the rest of the packet rather than merely losing waypoints.

## Verified against a live realm

Watcher held its connection while Testwolf walked 45 units:

```text
Testwolf (walking):    -8948.2, -131.7  ->  -8909.3, -109.2
Watcher  (replicated): player 0x32,
                       -8948.2, -131.7  ->  -8909.3, -109.2
                       (45.0 units over 67 applied updates)

applied: 24 object updates, 286 monster moves, 66 relayed moves, 0 undecodable
world:   97 objects (101 created, 50 recreated, 4 removed, 718 moves, 0 orphaned)
```

The observer's replicated position for another player matches that player's own
record exactly. Zero orphaned updates across 376 applied changes means no create
block was lost and no guid was misread, and `101 - 4 == 97` closes the books.

One caveat worth stating rather than glossing: **`Values` blocks are rare in a
quiet zone.** These sessions saw one apiece. The merge path is covered
thoroughly by unit tests, but it has had very little real traffic through it;
combat would be the way to exercise it properly.

## Names, and chat

Nothing in an object update carries a name. A player's comes only from
`SMSG_NAME_QUERY_RESPONSE` and a creature's only from
`SMSG_CREATURE_QUERY_RESPONSE`, each in answer to a query this client has to
send. A client that never asks shows a world of anonymous things.

Two facts about those responses are worth stating, because they signal the same
condition in completely different ways:

- The **name query** flags "no such player" with a separate byte after the
  packed guid, and the packet then stops.
- The **creature query** flags it with the *top bit of the entry itself*.
  Reading that as a plain entry gives a creature numbered two billion whose
  name is whatever bytes came next.

The creature response is parsed **in full** — every trailing model id, quest
item and movement id — even though only the name is wanted. Stopping after the
name would throw away the check that makes the name trustworthy: if the tail
does not line up, some earlier field was the wrong width, and the name read
before it is no better than the bytes that were skipped.

Declined names (a Russian-locale feature: five grammatical forms behind a
trailing flag) are skipped but still **read**, or `finish` reports them as
leftovers and a correct parse looks like a broken one.

### `SMSG_MESSAGECHAT` changes shape by its own first byte

The most layout-dependent packet this client parses. A creature's line carries
the speaker's name inline; a player's does not, and the guid is all that is
sent. A channel line carries the channel; an achievement line carries an id
*after* the tag, where nothing else has anything. A creature addressing another
creature names both; a creature addressing a player names only itself.

Every variant is the same handful of leading fields followed by something
different — the exact shape where a wrong guess parses perfectly. All of them
run through one cursor ending at `finish`, and reading a variant with the wrong
shape does not produce a slightly wrong message: it produces one whose text is
somebody's guid, with the leftover count as the only evidence.

Note also that the chat type goes **out** as a 32-bit value and comes **back**
as a single byte. Same field, two widths, one direction each.

## Two ways to be ignored, and how long they took to tell apart

Sending chat failed silently three times, and each failure looked identical
from the client: the packet went out, the session survived, and absolutely
nothing came back. No error, no notification, no reply. That is the worst
possible feedback, and it is worth recording what each cause actually was.

1. **`LANG_UNIVERSAL` is not a language you may speak.** It is what a GM
   command uses. An ordinary account sending it is refused with no reply.
   Chat has to go out in the character's *own* race language —
   `chat::language_for_race`. Getting this wrong does not garble the text;
   that is what happens when someone *hears* a language they lack. It stops
   the message being accepted at all.
2. **`CHAT_MSG_SAY` is `0x01`, not `0x00`. `0x00` is `CHAT_MSG_SYSTEM`.**
   A client claiming to be the server announcing something is not allowed to,
   so the message is dropped — silently, again. `CLAUDE.md` already recorded a
   result-code enum off by one as an earlier bug; this is the same mistake in
   a different table, and it cost more than the first one because the symptom
   was *nothing* rather than a wrong value.
3. **The line was received and thrown away by our own tooling.** A two-client
   test showed chat never arriving, when it had in fact arrived during a drain
   done for another reason whose report was discarded. `WorldState::replicate`
   has one dispatch table by design — but one table does not save a *caller*
   from ignoring a category it produces, and chat is returned rather than
   stored precisely so it cannot accumulate unbounded.

The lesson that generalises: when a send produces no reply at all, the first
thing to build is not a better guess at the layout but **an inventory of what
did arrive**. `wow-cli world --stay` now prints every opcode seen, decoded or
not, because "the server never sent it" and "it arrived and we could not read
it" are the same observation until something distinguishes them, and they want
opposite investigations.

## Verified against a live realm

- `--names` resolved 50 names across 131 replicated objects with 50 queries,
  50 answers and 0 unanswered. The count is lower than the object count because
  creatures are keyed by **entry**, not guid — a zone of forty wolves costs one
  query.
- `--say` produced `[say] Testwolf: hello from open-wow`, the server's own
  relay of the line back to its sender, attributed through the name cache.
- Two clients: `ACCOUNT34`'s `Watcher` whispered `ACCOUNT33`'s `Testwolf`, and
  the whisper arrived with `SMSG_MESSAGECHAT x1` in the opcode histogram. The
  structure went out through one client, through the server, and back in
  through another — the same evidence shape that closed 3.4 and 3.5.
- A **yell** between the same two characters, ~154 units apart, did *not*
  arrive, while the sender's own echo did. Chat delivery is range-limited and
  this realm's yell range is under 154 units, which is worth knowing before
  concluding a parser is broken: whisper is the only chat with no range, and
  therefore the only one that tests delivery without positions being part of
  the experiment.

## Open questions

An unidentified opcode **0x029D** arrives exactly once per movement packet sent,
with a one-byte body of `00`. The 1:1 correspondence and the payload are
consistent with a stand-state update being pushed on every movement packet, but
that is inference from two observations, not something confirmed, so it is not
named in `opcode.rs`.

Back-to-back sessions on one account are occasionally refused while the previous
one is still logging out. It is transient and retrying works.

## Not implemented yet

PIN and authenticator second factors (the client refuses rather than guessing).
On the world side: interpreting the update fields beyond the handful named in
`update.rs`. Spline paths are parsed exactly but discarded, because nothing
consumes them until movement prediction exists.

Interpreting the update fields beyond the handful named in `update.rs`, and
`SMSG_MONSTER_MOVE`, whose spline payload is skipped rather than followed.

## Spell damage

`SMSG_SPELLNONMELEEDAMAGELOG` (`0x0250`) is the other half of the combat log.
Melee arrives as `SMSG_ATTACKERSTATEUPDATE`; everything cast arrives here, and
until now this client counted the opcode and dropped it.

**Captured before it was parsed**, which is the rule this project adopted after
`SMSG_SPELL_START`. A level-one druid put a Wrath into a Young Nightsaber:

```
wow-cli world <host> --user <acct> --enter Testdruid \
  --target "Nightsaber" --cast 5176 --stay 8 --capture wrath.txt
```

The 46-byte body anchors five fields at once, which is what makes it a
measurement rather than a layout that happens to parse:

| field | value | why it is not a guess |
|---|---|---|
| target guid | `0xf1300007ef06c111` | the creature that was selected |
| caster guid | `0x33` | the druid casting |
| spell id | 5176 | the spell that was asked for |
| damage | 17 | right for a rank-one Wrath |
| school | 8 | Nature, and Wrath is a Nature spell |

**The twenty bytes after the school are kept and not named.** They are all zero
in the only capture. They are widely said to be absorb, resist, a periodic flag,
block and hit info, and that may be right -- but a partly resisted hit is
exactly the packet nobody here has captured, and a wrong *name* on a combat log
misexplains a fight to whoever reads it next. `Reader::rest` takes them so
`finish` still asserts the body was consumed exactly: a parser that ignored the
tail would be making no claim at all, where this one claims "I know this much
and no more". The first non-zero one settles it.

For the same reason no critical flag is read, and a spell number is never
coloured as a critical: whatever marks one lives in those bytes.

### Two things the rig taught

**The nearest unit is usually the wrong one.** `--select` picks the closest
thing, which next to a starting character is a friendly quest giver, and a
damage spell aimed at one is refused with "cannot be cast on that target" --
indistinguishable from a malformed cast. `--target <name>` picks by name
instead, resolving names first, because until the queries come back every unit
is a bare guid.

**A cast at a wandering creature has to be retried.** The same spell at the same
creature was refused `0x61` at 22 units and accepted at 44 -- so not range, and
not facing either, since the client turns first. Line of sight is what changed:
the creature moved out from behind a tree. `--attack` learned the same lesson
about swings and re-swings until they land; `--cast` now re-casts up to four
times, re-facing before each. It is still not reliable, because the caster
never *moves* -- the approach loop `--attack` has is what would fix it, and
that is worth doing before the next capture of this shape.

**The cast's own drain now feeds `--capture`.** It was being counted and
dropped, which is exactly the failure recorded for `SMSG_ATTACKERSTATEUPDATE`:
the one packet that could answer the question, seen and lost. A spell's reply --
the damage log, the threat update -- lands in that drain and nowhere else.

## Death, release and the corpse run

Three states, not two: alive, dead-where-you-fell (no corpse *object* exists
yet), and ghost-at-the-graveyard (released, with a corpse object now
somewhere in the world to run back to). `docs/ROADMAP.md`'s 4.4 section has
the narrative and the three bugs that shaped it; this is the shape of the
messages themselves.

| message | direction | body |
|---|---|---|
| `CMSG_REPOP_REQUEST` `0x015A` | out | one byte, read and discarded server-side |
| `MSG_MOVE_TELEPORT_ACK` `0x00C7` | both | in: `{packed guid, u32 counter, MovementInfo}`; out: `{packed guid, u32 counter (echoed), u32 tick}` |
| `MSG_CORPSE_QUERY` `0x0216` | both | out: empty; in: `{u8 0}` (no corpse) or `{u8 1, i32 map, f32 x, f32 y, f32 z, i32 corpseMap, u32 unknown}` |
| `CMSG_RECLAIM_CORPSE` `0x01D2` | out | corpse guid, **unpacked** -- eight plain bytes, unlike almost every other guid this protocol sends |
| `SMSG_DEATH_RELEASE_LOC` `0x0378` | in | `{u32 map, f32 x, f32 y, f32 z}`; map `0xFFFFFFFF` means "clear the marker", not a destination |
| `SMSG_CORPSE_RECLAIM_DELAY` `0x0269` | in | `u32` milliseconds |

Parsers live in `crates/world/src/death.rs`; the writes are
`Connection::release_spirit`, `Connection::reclaim_corpse`,
`Connection::query_corpse` and `Connection::acknowledge_teleport` in
`crates/world/src/client.rs`.

**Both writes refuse in total silence -- seven separate conditions between
them, none of which says which.** `wow-cli world --release` and `--reclaim`
therefore report a before-and-after of the things that must change
(`PLAYER_FLAGS`' ghost bit, health, the corpse object appearing) rather than
trusting an ack that never comes.

**An unacknowledged teleport makes the server discard every movement packet
from that client until it arrives.** A release moves the ghost to the
graveyard server-side and then waits for `MSG_MOVE_TELEPORT_ACK` before
believing the client noticed. Skip it and the ghost never leaves the corpse
as far as the server is concerned: a range check against the reclaim then
passes at nought yards no matter how far away the client actually walked,
which is exactly the bug that let a reclaim succeed from 58 yards when the
server's own limit is 39. The same obligation applies to any teleport, not
only a release -- a client that ignores it freezes server-side while its own
camera keeps walking around locally.

**The reclaim delay stacks rather than being a constant.** Observed at 30s on
a first death, 60s on the second, 120s on the third. Code that hardcoded
thirty seconds would have worked exactly once per character -- precisely as
often as it would have been tested.

**A graveyard accumulates more than one corpse-shaped object, and picking by
owner guid picks wrong.** Corpse objects include the bones of bodies already
reclaimed, and bones carry the same owner guid as the current body -- one run
saw nine corpse-shaped objects at a graveyard, five of them belonging to the
same character. `MSG_CORPSE_QUERY` is the only way to know which one is
current; the replicated objects then contribute only the guid, chosen as
whichever is nearest the query's answer.

**The graveyard needs no `WorldSafeLocs.dbc`.** `SMSG_DEATH_RELEASE_LOC`
carries the map and position the server already chose. The obvious design --
look up the nearest row and walk there -- would put a table lookup on the
critical path of a feature that needs none; the table (now transcribed) is
wanted only to put a *name* on a place the packet already gives coordinates
for.

## Talking to an NPC

`CMSG_GOSSIP_HELLO` (`0x017B`) carries the NPC's guid **unpacked** -- eight
plain bytes, like `CMSG_LOOT` -- and `SMSG_GOSSIP_MESSAGE` (`0x017D`) answers
with the menu.

**This is where the evidence changes shape.** Everything up to here could be
checked against a table shipped with the client. Gossip text, menu options and
quest titles are in the *server's* world database and reach the client only in
this packet, so there is no `Item.dbc` to pair a field against. What stands in
for it on a test realm is that the world database is readable, and is a source
the client is never sent -- which is the same class of evidence, and is what
every field below was confirmed with.

```
u64 npc guid
u32 menu id            creature_template.gossip_menu_id
u32 greeting text id   gossip_menu.TextID -- resolved by a query, not by us
u32 option count
  u32 index            gossip_menu_option.OptionID -- see below
  u8  icon
  u8  coded            whether choosing it opens a text box
  u32 money            copper the option costs
  cstring message      the line the player reads
  cstring box message  the confirmation text, when there is one
u32 quest count
  u32 quest id         quest_template.ID
  u32 icon             available, taken, or ready to hand in
  i32 level            signed: -1 means "scales to the player"
  u32 flags            quest_template.Flags
  u8  repeatable
  cstring title        quest_template.LogTitle
```

Confirmed on three NPCs chosen so the two counts differ -- Innkeeper Farley (3
options, 0 quests, 136 bytes), Marshal McBride (0 and 0, 24 bytes) and Deputy
Willem (0 options, 1 quest, 57 bytes). One sample proves very little here: most
of a gossip menu is zeroes, and a reading with the quest block in the wrong
place parses Farley's packet perfectly, because Farley offers no quests.

### An option index is the server's id, not a row number

Menu 1291 has four rows in `gossip_menu_option`. Three arrived, carrying indices
**1, 2 and 3** -- the server filtered out option 0, a Hallowe'en seasonal line,
and did not close the numbering up.

So a reply must carry the index the server sent, never a position in whatever
list the client built. Identical to `SMSG_LOOT_RESPONSE`'s loot slots, and with
the same failure mode: a client that renumbers works fine everywhere except at
NPCs whose menus are conditional.

### An empty quest list is a statement

Greeting a questgiver and getting zero quests looks exactly like the quest block
being misplaced. It was correct: McBride's whole chain is gated behind `A Threat
Within`, which Deputy Willem gives out. Before concluding a block is wrong,
check that the character asking could have exhibited it.

### Not sent yet

`CMSG_GOSSIP_SELECT_OPTION`, so the menu can be read and not clicked. The
greeting text id resolves to nothing without the `npc_text` query. Vendor lists,
buying, selling and the quest flow past this one-line summary are all untouched.
