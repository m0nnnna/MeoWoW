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

## Not implemented yet

PIN and authenticator second factors (the client refuses rather than guessing),
and everything past the realm list: the world server handshake, its RC4 header
cipher, and the opcode protocol.
