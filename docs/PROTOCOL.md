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

## Not implemented yet

PIN and authenticator second factors (the client refuses rather than guessing).
On the world side: entering the world, the object update fields, and movement.
`CMSG_PING` is defined but not yet sent, so a long-idle connection will be
dropped by the server.
