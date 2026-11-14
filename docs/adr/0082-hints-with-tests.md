# 0082. Hints with tests, their ids sealed

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-41; amends C-38; relies on C-39, C-40, ADR 0075, 0080, 0081, RFC 0004

## Context

A machine with an empty cache, a CI runner or a sandbox, knows no test of a test file until it has
run it, so with tests as the unit it reuses nothing one by one on its first run: ADR 0075's hints
carry what test files read, not their tests. RFC 0004 §3.5 puts tests in hints. A hint carries no
path in the clear (C-38: every path is a keyed hash), and a test's id says as much as a path:
`test_refund_for_acme_invoice[eu-vat]`. Ids can't be tokens, since a reader has no list of ids to
match them against, as it has the scope's files for tokens. They are encrypted instead, under a key
the server never has.

## Decision

1. **Version 1** of a hint's content, `hushrun.unit-hints/v1`, is version 0's (C-38) followed by
   tests' own lists, each a sorted list of indices into the hint's token table (files read, then
   directories listed), sorted and each once; then the **sealed tests**. A hint with no tests is
   written as version 0, byte for byte as before, so its content id doesn't change.
2. **Sealed** with XChaCha20-Poly1305 (RFC 8439's AEAD with HChaCha20's 192-bit nonce) under the
   **seal key**: HMAC-SHA-256 with the ASCII bytes `hushrun.unit-hints-seal/v1` as its key, of the
   bytes `shape` hashes, as the path key is derived (C-38) but under another label, so neither key
   says anything of the other. The associated data is the content before the sealed part: the seal
   covers the adapter, the tokens, the units and the lists. The plaintext holds, for each unit with
   tests, its position, then each test's id (UTF-8, 1 to 4,096 bytes, strictly ascending) and the
   index of its list or `0xFFFFFFFF` for its test file's.
3. **A nonce that never repeats under one key, and one content id per hint.** The nonce is the
   first 24 bytes of HMAC-SHA-256 under the seal key of `hushrun.unit-hints-nonce/v1`, a line feed,
   the associated data and the plaintext: the same hint seals to the same bytes, so content
   addressing stays true (the same hint is the same content and isn't sent twice), and two hints
   that differ in anything have different nonces. A synthetic nonce is safe with XChaCha20's 192
   bits, where a 96-bit nonce derived this way would be too short to rule out a collision.
4. **The crate** is RustCrypto's `chacha20poly1305` 0.11: maintained, pure Rust, constant time
   without AES instructions (so the same on every runner, unlike AES-GCM on machines without them),
   with XChaCha20 for the 192-bit nonce, and its `chacha20` dependency already in the lock. It adds
   eight small crates, all MIT or Apache-2.0; `cargo deny check` passes and the notices are
   regenerated.
5. **What a hint holds of tests** (C-41). With `reads: test`, every test with a key (one that
   passed or failed, never one not followed or not run) and its own list, or none when it read its
   test file's. With `reads: file`, ids alone, and only of test files that didn't pass whole: one
   that did records no test's key. A test file reused in part is now part of the hint, as one that
   ran is.
6. **Too large**: a writer leaves out tests' lists first, then tests, and says so once:
   `<check>: what each of its tests read wasn't shared with other machines: it is too large, so they
   take each test to have read what its test file read`, or `<check>: its tests weren't shared with
   other machines: there are too many, so they reuse them by test file`.
7. **A reader never fails on tests.** A seal that doesn't open (another check's key, a flipped
   bit), or a plaintext that isn't the one canonical plaintext, reads as a hint with no tests: its
   units stand. A test whose list names a token that matches nothing here is left out; the others
   stay. Version 0 hints read as before.
8. **Readers use them** through the one function (C-40): with `reads: file` a hint's test is keyed
   by its test file's lists; with `reads: test` by its own list, after this machine's. A test
   reused on another machine's list keeps that pass as its row here.
9. `tools/refdigest.py` implements XChaCha20-Poly1305 in the standard library alone, checks it
   on draft-irtf-cfrg-xchacha's example, and recomputes the new vectors in
   `testdata/digest/unit-hints.json` (`pytest-tests`, `pytest-tests-file-gone`, `noTests`, two more
   unread); the fuzz target holds tests read to the same bound as units.

## Consequences

- A fresh machine reuses single tests from its first run: with `reads: file` those of a test file
  one of whose tests failed elsewhere; with `reads: test` each test on what another machine saw it
  read.
- The server learns how many tests a hint holds, roughly, from its size, and nothing of their ids.
- A hint with tests is larger; past 2 MiB compressed its lists, then its tests, are left out.

## Alternatives considered

- **Ids as keyed hashes, like paths:** a reader can't match a hash against ids it doesn't know.
- **A random nonce:** the same hint would seal to other bytes each time, so its content id would
  change, the server would keep copies, and a machine would send again what it had.
- **AES-256-GCM-SIV:** misuse-resistant too, but not constant time without AES instructions, and a
  larger dependency; XChaCha20 with a derived nonce gives the same property here.
- **One key for paths and ids:** the seal key is derived under its own label, so a use of one never
  bears on the other.
