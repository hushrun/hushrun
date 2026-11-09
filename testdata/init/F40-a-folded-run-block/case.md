# F40: commands in folded `run: >` blocks

**Pattern.** A `run: >-` block that folds one `cargo test` over three lines; another that folds a
`hushrun run` line over two, as a person might write a long wrap; and a `run: >` block whose
second line is indented further.

**Mirrors.** hushrun's own repository, whose registry tests a session wrapped over three folded
lines.

**Expected after ADR 0055.** YAML makes the line breaks between lines at a block's indentation
spaces, so the shell gets one line. init wraps the first block's `cargo test` once, in front of
its first line, with its options from all three; it reads the second as through hushrun already,
and adds no second `hushrun run` inside it. A line indented further keeps its line break, so the
third block runs `cargo fmt --all` and then `--check`: no check, as the shell reads it. Before,
init read each line alone: it told the first block's check by its first line only, and wrapped
the second block's `cargo clippy` again, inside its `hushrun run`.
