# F42: a line a comment leaves

**Pattern.** A job that tests against public-good Sigstore, whose services and trust root change
while the code doesn't, marked `# hushrun: leave, <why>` above its step; `cargo deny check # hushrun:
leave` at the end of a line; and, in a `run: |` block, a comment above one of its commands.

**Mirrors.** hushrun's own repository, which keeps its public-good Sigstore test unwrapped.

**Expected after ADR 0055.** init leaves each marked line as it is, with `a comment says to leave
it`, and the comment's reason after it when there is one. The Sigstore job gets nothing: no setup
and no ID token, since nothing else of it is wrapped. The block's other command is wrapped, and so
is the unmarked `cargo test` beside `cargo deny`.
