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

**Being seen moving by another client is not yet proven.** It needs a second
account logged in at the same time, and only one is available here. The inbound
half is written and wired up — a relayed `MSG_MOVE_*` is decoded and its mover
reported — but with nobody else online it has never received a real packet. What
*is* proven is that the server accepts our movement and persists it, which is
the half that had to be right first.
