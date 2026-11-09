//! Which package.json scripts are checks, and how init puts `hushrun run` in front of them
//! (C-15).
//!
//! A script is a shell command line, which npm, pnpm, yarn and bun run with `sh -c` (cmd.exe on
//! Windows). Init wraps only what it can read for sure: simple commands joined by `&&`, each
//! wrapped on its own. Anything else a shell does (`||`, `;`, pipes, redirections,
//! substitutions, a leading `VAR=value`) leaves the script as it is, with the reason.

use std::ops::Range;

use super::checks::{only_shows_output, without_launchers};

/// What kind of check a script is: the order init shows a skip in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Lint,
    Typecheck,
    Test,
}

/// The scripts init looks at, and their kind.
pub const NAMES: &[(&str, Kind)] = &[
    ("test", Kind::Test),
    ("test:unit", Kind::Test),
    ("lint", Kind::Lint),
    ("typecheck", Kind::Typecheck),
    ("type-check", Kind::Typecheck),
    ("types", Kind::Typecheck),
    ("check-types", Kind::Typecheck),
    ("check", Kind::Typecheck),
    ("format:check", Kind::Lint),
    ("tsc", Kind::Typecheck),
];

/// The kind of a script init looks at, by its name.
pub fn kind(name: &str) -> Option<Kind> {
    NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, kind)| *kind)
}

/// What init does with a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Wrap it: the new command line. `vitest_run` says a bare `vitest` became `vitest run`.
    Wrap { command: String, vitest_run: bool },
    /// It already runs through `hushrun run`.
    Wrapped,
    /// Leave it as it is, for this reason.
    Leave(String),
}

/// What init does with the script `command`.
pub fn decide(command: &str) -> Decision {
    decide_with(command, &|_| false)
}

/// What init does with the script `command`, where `fans_out` tells a command that runs scripts
/// of a workspace's packages, which init wraps in each package, not here (ADR 0051).
pub fn decide_with(command: &str, fans_out: &dyn Fn(&[&str]) -> bool) -> Decision {
    if command.trim().is_empty() {
        return Decision::Leave("it is empty".into());
    }
    if command.contains("hushrun run") {
        return Decision::Wrapped;
    }
    if command.contains("no test specified") {
        return Decision::Leave("it is npm's placeholder".into());
    }
    let pieces = match lex(command, false) {
        Ok(pieces) => pieces,
        Err(reason) => return Decision::Leave(reason),
    };
    let mut segments: Vec<Vec<&Word>> = vec![Vec::new()];
    for piece in &pieces {
        match piece {
            Piece::Word(word) => segments.last_mut().expect("one segment").push(word),
            Piece::Op("&&") => segments.push(Vec::new()),
            Piece::Op(op) => return Decision::Leave(operator(op)),
        }
    }
    if segments.iter().any(Vec::is_empty) {
        return Decision::Leave("a command of its && chain is empty".into());
    }
    for words in &segments {
        let first = words[0].text.as_str();
        if assignment(first) && !words[0].quoted {
            return Decision::Leave(format!("it sets {first} first"));
        }
        if BUILTINS.contains(&first) {
            return Decision::Leave(format!("it uses the shell's {first}"));
        }
        for word in words {
            if let Some(reason) = watches_or_writes(&word.text) {
                return Decision::Leave(reason);
            }
        }
        if let Some(reason) = watches_by_default(words) {
            return Decision::Leave(reason);
        }
    }
    let wrapped: Vec<&Vec<&Word>> = segments
        .iter()
        .filter(|words| {
            let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
            !package_manager(words) && !fans_out(&texts)
        })
        .collect();
    if wrapped.is_empty() {
        return Decision::Leave("it only runs other scripts or the package manager".into());
    }
    // Insertions into the command, from its end, so that earlier offsets hold.
    let mut inserts: Vec<(usize, &str)> = Vec::new();
    let mut vitest_run = false;
    for words in wrapped {
        inserts.push((words[0].span.start, "hushrun run -- "));
        if let Some(vitest) = bare_vitest(words) {
            inserts.push((vitest.span.end, " run"));
            vitest_run = true;
        }
    }
    inserts.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let mut wrapped = command.to_string();
    for (at, text) in inserts {
        wrapped.insert_str(at, text);
    }
    Decision::Wrap {
        command: wrapped,
        vitest_run,
    }
}

/// One `hushrun run` of a script, as its key needs it (C-9): the name it gives, if any, and its
/// command, word by word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub name: Option<String>,
    /// `--scope`'s, when it gives one.
    pub scope: Option<String>,
    pub command: Vec<String>,
    /// \[ADR 0050\] CI or the shell fills in some of its command's words, `${{ … }}` or `$NAME`, so
    /// that only the run knows its key: `command` holds them as written.
    pub filled_in: bool,
}

impl Run {
    /// A command line that starts this run again, quoted for a shell, with `program` for
    /// `hushrun`.
    pub fn line(&self, program: &str) -> String {
        let mut words = vec![program.to_string(), "run".to_string()];
        if let Some(scope) = &self.scope {
            words.extend(["--scope".to_string(), scope.clone()]);
        }
        words.extend(self.name.clone());
        words.push("--".to_string());
        words.extend(self.command.iter().map(|word| quoted(word)));
        words.join(" ")
    }
}

/// `word` as a shell reads it back.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    match plain {
        true => word.to_string(),
        false => format!("'{}'", word.replace('\'', "'\\''")),
    }
}

/// The `hushrun run`s of the script `command`, in order, or `None` when its words aren't sure.
/// `hushrun ready` looks each one up by its key, so a word the shell would change, an option of
/// `hushrun run`, or an operator other than `&&` means it can't tell which key the script runs
/// under (C-22). Other commands of a `&&` chain aren't hushrun's, and don't count.
pub fn runs(command: &str) -> Option<Vec<Run>> {
    let pieces = lex(command, false).ok()?;
    let mut segments: Vec<Vec<&Word>> = vec![Vec::new()];
    for piece in &pieces {
        match piece {
            Piece::Word(word) => segments.last_mut().expect("one segment").push(word),
            Piece::Op("&&") => segments.push(Vec::new()),
            Piece::Op(_) => return None,
        }
    }
    let mut runs = Vec::new();
    for words in &segments {
        match run_of(words) {
            Reading::NotARun => {}
            Reading::Sure(run) if !run.filled_in => runs.push(run),
            Reading::Sure(_) | Reading::Unsure(_) => return None,
        }
    }
    Some(runs)
}

/// What a command's words say about a `hushrun run`.
enum Reading {
    NotARun,
    Sure(Run),
    /// It is one, but its key can't be told from its words: why.
    Unsure(&'static str),
}

fn run_of(words: &[&Word]) -> Reading {
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let Some(after) = after_hushrun_run(&texts) else {
        return Reading::NotARun;
    };
    const OPTIONS: &str = "it gives an option hushrun can't read a key from";
    let Some(separator) = after.iter().position(|word| *word == "--") else {
        return Reading::Unsure("it has no -- before its command");
    };
    let first = words.len() - after.len();
    let (mut name, mut scope) = (None, None);
    let mut at = 0;
    while at < separator {
        let word = after[at];
        if the_shell_changes(words[first + at]) {
            return Reading::Unsure("the shell makes its words what they are");
        }
        if word == "--scope" {
            at += 1;
            if at == separator {
                return Reading::Unsure(OPTIONS);
            }
            if the_shell_changes(words[first + at]) {
                return Reading::Unsure("the shell makes its words what they are");
            }
            scope = Some(after[at].to_string());
        } else if let Some(value) = word.strip_prefix("--scope=") {
            scope = Some(value.to_string());
        } else if word.starts_with('-') || name.is_some() {
            return Reading::Unsure(OPTIONS);
        } else {
            name = Some(word.to_string());
        }
        at += 1;
    }
    let command = &words[first + separator + 1..];
    if command.is_empty() {
        return Reading::Unsure("it has no command");
    }
    if command.iter().any(|word| word.glob) {
        return Reading::Unsure("the shell makes its words what they are");
    }
    Reading::Sure(Run {
        name,
        scope,
        command: command.iter().map(|word| word.text.clone()).collect(),
        filled_in: command.iter().any(|word| fills_in(word)),
    })
}

/// One `hushrun run` of a CI file's shell lines, and the directory it starts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRun {
    /// The run, or why hushrun can't tell its key.
    pub run: Result<Run, &'static str>,
    /// Below the directory the lines start in, `/`-separated; `None` when a `cd` moved
    /// somewhere hushrun can't follow.
    pub moved: Option<String>,
}

/// The `hushrun run`s of `commands`, a CI step's shell lines, in order. Anything else a shell does
/// around them doesn't matter, but a run whose words the shell or CI fills in has no key to look
/// up: its `run` says so.
pub fn line_runs(commands: &str) -> Vec<LineRun> {
    let Ok(pieces) = lex(commands, true) else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    let mut here = Some(String::new());
    let mut words: Vec<&Word> = Vec::new();
    for piece in pieces.iter().chain([&Piece::Op(";")]) {
        match piece {
            Piece::Word(word) => words.push(word),
            Piece::Op(op) => {
                let command: Vec<&Word> = words.drain(..).skip_while(|word| word.assigns).collect();
                if let [cd, dir, ..] = command.as_slice()
                    && cd.text == "cd"
                {
                    here = here.and_then(|from| moved(&from, dir));
                }
                match run_of(&command) {
                    Reading::NotARun => {}
                    Reading::Sure(run) => runs.push(LineRun {
                        run: Ok(run),
                        moved: here.clone(),
                    }),
                    Reading::Unsure(why) => runs.push(LineRun {
                        run: Err(why),
                        moved: here.clone(),
                    }),
                }
                // A subshell's `cd` ends with it, which a line-by-line read can't follow.
                if *op == "(" {
                    here = None;
                }
            }
        }
    }
    runs
}

/// \[ADR 0044\] The NAMEs of the `hushrun run`s of `commands`, a CI job's shell lines, as CI's file
/// writes them: `${{ … }}` stays, without its spaces, in the word CI fills it into.
pub fn written_names(commands: &str) -> Vec<String> {
    let Ok(pieces) = lex(&joined_expressions(commands), true) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    for piece in pieces.iter().chain([&Piece::Op(";")]) {
        match piece {
            Piece::Word(word) => words.push(&word.text),
            Piece::Op(_) => {
                let command: Vec<&str> = words
                    .drain(..)
                    .skip_while(|word| assignment(word))
                    .collect();
                names.extend(written_name(&command));
            }
        }
    }
    names
}

/// The NAME of a command's `hushrun run`, when it has one and isn't a stage's.
fn written_name(words: &[&str]) -> Option<String> {
    let after = after_hushrun_run(words)?;
    let separator = after.iter().position(|word| *word == "--")?;
    let mut name = None;
    let mut options = after[..separator].iter();
    while let Some(word) = options.next() {
        match *word {
            "--scope" | "--rev" | "--digest" => {
                options.next();
            }
            word if word == "--stage" || word.starts_with("--stage=") => return None,
            word if !word.starts_with('-') => name = Some(word.to_string()),
            _ => {}
        }
    }
    name
}

/// `text`, with the spaces of each `${{ … }}` taken out.
fn joined_expressions(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        let Some(end) = rest[start..].find("}}").map(|end| start + end + 2) else {
            break;
        };
        joined.push_str(&rest[..start]);
        joined.extend(rest[start..end].chars().filter(|c| !c.is_whitespace()));
        rest = &rest[end..];
    }
    joined.push_str(rest);
    joined
}

/// Where `cd dir` from `from` goes, when hushrun can tell.
fn moved(from: &str, dir: &Word) -> Option<String> {
    if the_shell_changes(dir) || dir.text.starts_with('/') || dir.text.starts_with('-') {
        return None;
    }
    let mut parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    for part in dir.text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// One command of a CI file's shell line, as `hushrun init` might wrap it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub words: Vec<String>,
    /// Where it starts in the line.
    pub at: usize,
    /// Why it can't be wrapped, whatever it is.
    pub leave: Option<String>,
    /// Below the directory the lines start in; `None` after a `cd` hushrun can't follow.
    pub moved: Option<String>,
    /// \[ADR 0050\] It starts with `NAME=value` words, which a bash step can wrap through `env`.
    pub prefixed: bool,
    /// \[ADR 0050\] CI or the shell fills in some of its words, `${{ … }}` or `$NAME`: it wraps
    /// when each is safe to record (`expansions`).
    pub fills_in: bool,
}

/// \[ADR 0050\] Why a command whose words a command's output fills in is left: its key and its
/// record would hold whatever that printed.
pub const SUBSTITUTED: &str = "a command fills in its words";

/// The commands of `lines`, a CI step's shell lines, each with where it starts, and `here`, where
/// a `cd` left the lines before them. Only a chain of `&&` wraps, and a last `|` into what only
/// shows the output: any other operator on a line leaves its commands, each with the reason.
pub fn commands(line: &str, here: &mut Option<String>) -> Vec<Found> {
    match lex(line, false) {
        Ok(pieces) => commands_of(&pieces, here),
        Err(_) => Vec::new(),
    }
}

/// \[ADR 0050\] [`commands`], as init reads them to wrap: a line where a command's output fills
/// in words, `$(…)` or backticks, still has its commands, and each that holds one is left.
pub fn init_commands(line: &str, here: &mut Option<String>) -> Vec<Found> {
    match lex(line, false) {
        Ok(pieces) => commands_of(&pieces, here),
        Err(why) if why == SUBSTITUTION => {
            let Ok(pieces) = lex(line, true) else {
                return Vec::new();
            };
            let mut found = commands_of(&pieces, here);
            for command in &mut found {
                if command
                    .words
                    .iter()
                    .any(|word| word.contains("$(") || word.contains('`'))
                {
                    command.leave = command.leave.take().or(Some(SUBSTITUTED.to_string()));
                }
            }
            found
        }
        Err(_) => Vec::new(),
    }
}

/// The commands of a line read as `pieces`, as [`commands`] says them.
fn commands_of(pieces: &[Piece], here: &mut Option<String>) -> Vec<Found> {
    // What goes through a pipe only to be shown, last on the line, stops nothing: `hushrun run` in
    // front of the command prints what it prints, and a skip leaves it nothing to show.
    let shown = |at: usize| {
        let rest: Option<Vec<&str>> = pieces[at + 1..]
            .iter()
            .map(|piece| match piece {
                Piece::Word(word) => Some(word.text.as_str()),
                Piece::Op(_) => None,
            })
            .collect();
        rest.is_some_and(|words| only_shows_output(&words))
    };
    let blocked = pieces
        .iter()
        .enumerate()
        .find_map(|(at, piece)| match piece {
            Piece::Op("&&") => None,
            Piece::Op("|") if shown(at) => None,
            Piece::Op(op) => Some(operator(op)),
            Piece::Word(_) => None,
        });
    let mut found = Vec::new();
    let mut segments: Vec<Vec<&Word>> = vec![Vec::new()];
    for piece in pieces {
        match piece {
            Piece::Word(word) => segments.last_mut().expect("one segment").push(word),
            Piece::Op("&&") => segments.push(Vec::new()),
            Piece::Op(_) => segments.push(Vec::new()),
        }
    }
    for words in segments.iter().filter(|words| !words.is_empty()) {
        if let [cd, dir, ..] = words.as_slice()
            && cd.text == "cd"
        {
            *here = here.take().and_then(|from| moved(&from, dir));
            continue;
        }
        let first = &words[0];
        // \[ADR 0050\] Words CI or the shell fills in, and variables set first, are for init to
        // judge, with what the file says of them.
        let leave = blocked.clone().or_else(|| {
            if words.iter().any(|word| word.glob) {
                return Some("the shell makes its words what they are".to_string());
            }
            // \[ADR 0050\] What the command after a launcher and its options says, so that `uv run
            // --group dev pytest` isn't a dev server.
            let texts: Vec<&str> = words
                .iter()
                .skip_while(|word| word.assigns)
                .map(|word| word.text.as_str())
                .collect();
            without_launchers(&texts)
                .iter()
                .find_map(|word| watches_or_writes(word))
        });
        found.push(Found {
            words: words.iter().map(|word| word.text.clone()).collect(),
            at: first.span.start,
            leave,
            moved: here.clone(),
            prefixed: first.assigns,
            fills_in: words.iter().any(|word| fills_in(word)),
        });
    }
    found
}

/// What follows `hushrun run` in a command's words, however the package manager starts it.
fn after_hushrun_run<'w, 'a>(words: &'w [&'a str]) -> Option<&'w [&'a str]> {
    let start = match words {
        ["hushrun", "run", ..] => 0,
        ["npx" | "bunx" | "yarn" | "pnpm", "hushrun", "run", ..] => 1,
        ["npx", "--no-install" | "--no", "hushrun", "run", ..]
        | ["pnpm" | "yarn" | "npm", "exec", "hushrun", "run", ..] => 2,
        _ => return None,
    };
    Some(&words[start + 2..])
}

/// Whether a shell, or CI before it, would make this word something else before hushrun saw it.
fn the_shell_changes(word: &Word) -> bool {
    word.glob || fills_in(word)
}

/// \[ADR 0050\] Whether CI or the shell fills in some of this word: `${{ … }}`, `$NAME`, or a
/// command's output.
fn fills_in(word: &Word) -> bool {
    word.dollar || !word.expressions.is_empty()
}

/// The scripts that shell commands run through a package manager, in order, such as `test`
/// for `npm test`; none after a `cd` elsewhere, whose scripts are another package's. \[ADR 0044\]
/// A `hushrun run … -- npm test` runs it too, unless CI fills in that run's name: see
/// `scripts_in_named_runs`.
pub fn scripts_run(commands: &str) -> Vec<String> {
    scripts_of(commands).0
}

/// \[ADR 0044\] The scripts that shell commands run inside a `hushrun run` whose words CI fills in.
/// Such a run has a key of its own in each variant, so what it starts is its business.
pub fn scripts_in_named_runs(commands: &str) -> Vec<String> {
    scripts_of(commands).1
}

/// The scripts of `commands`, then those inside a run whose words CI fills in.
fn scripts_of(commands: &str) -> (Vec<String>, Vec<String>) {
    let Ok(pieces) = lex(commands, true) else {
        return Default::default();
    };
    let (mut scripts, mut named) = (Vec::new(), Vec::new());
    let mut command: Vec<&str> = Vec::new();
    for piece in pieces.iter().chain([&Piece::Op(";")]) {
        match piece {
            Piece::Word(word) => command.push(&word.text),
            Piece::Op(_) => {
                let words: Vec<&str> = command
                    .drain(..)
                    .skip_while(|word| assignment(word))
                    .collect();
                match words.as_slice() {
                    ["cd" | "pushd", dir, ..] if !matches!(*dir, "." | "./") => break,
                    _ => {
                        let (started, filled_in) = past_hushrun_run(&words);
                        let script = super::manager::script_of(started);
                        match filled_in {
                            true => named.extend(script),
                            false => scripts.extend(script),
                        }
                    }
                }
            }
        }
    }
    (scripts, named)
}

/// \[ADR 0051\] Each simple command of `commands`, run from `dir`, with the directory it runs
/// in, past what sets variables in front of it and past a `hushrun run … --`: none after a `cd`
/// hushrun can't follow, nor inside a run whose words CI fills in, whose scripts are its own.
pub fn simple_commands(commands: &str, dir: &str) -> Vec<(String, Vec<String>)> {
    let Ok(pieces) = lex(commands, true) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut here = dir.to_string();
    let mut command: Vec<&Word> = Vec::new();
    for piece in pieces.iter().chain([&Piece::Op(";")]) {
        let Piece::Op(_) = piece else {
            let Piece::Word(word) = piece else {
                unreachable!("a piece is a word or an operator")
            };
            command.push(word);
            continue;
        };
        let words: Vec<&Word> = command.drain(..).skip_while(|word| word.assigns).collect();
        match words.as_slice() {
            [] => {}
            [cd, dir, ..] if matches!(cd.text.as_str(), "cd" | "pushd") => {
                match moved(&here, dir) {
                    Some(moved) => here = moved,
                    None => break,
                }
            }
            [cd] if cd.text == "cd" => break,
            _ => {
                let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
                let (started, filled_in) = past_hushrun_run(&texts);
                if !filled_in && !started.is_empty() {
                    found.push((
                        here.clone(),
                        started.iter().map(|w| (*w).to_string()).collect(),
                    ));
                }
            }
        }
    }
    found
}

/// The command a `hushrun run` of `words` starts, and whether CI fills in the run's words before
/// `--`; `words` themselves when they are no such run.
fn past_hushrun_run<'w, 'a>(words: &'w [&'a str]) -> (&'w [&'a str], bool) {
    let found = after_hushrun_run(words)
        .and_then(|after| Some((after, after.iter().position(|word| *word == "--")?)));
    match found {
        Some((after, separator)) => (
            &after[separator + 1..],
            after[..separator].iter().any(|word| word.contains("${{")),
        ),
        None => (words, false),
    }
}

/// A word of a command line, unquoted, and where it is in the line, quotes included.
#[derive(Debug, Clone)]
struct Word {
    text: String,
    span: Range<usize>,
    quoted: bool,
    /// \[ADR 0050\] The `${{ … }}` it holds, as written, which GitHub fills in before the shell.
    expressions: Vec<String>,
    /// `$` or a backtick outside them: the shell fills in a variable, or a command's output.
    dollar: bool,
    /// Unquoted, it has what a shell expands into other words: `*`, `?`, `[`, `{`, `%`, or `~`.
    glob: bool,
    /// \[ADR 0050\] It sets a variable, `NAME=value`, as the shell reads one: its name, up to the
    /// `=`, plain and unquoted, so `NAME="a b"` does and `"NAME=a"` doesn't.
    assigns: bool,
}

#[derive(Debug, Clone)]
enum Piece {
    Word(Word),
    /// `&&`, `||`, `|`, `;`, `&`, a redirection (`>` or `<`), a parenthesis, or a newline.
    Op(&'static str),
}

/// The shell's words and operators in `line`, or why init can't read it for sure. `lenient`
/// reads what it can instead: a substitution is part of its word, and quotes that don't close
/// end with the line.
fn lex(line: &str, lenient: bool) -> Result<Vec<Piece>, String> {
    let (masked, expressions) = masked(line);
    let mut pieces = lex_masked(&masked, lenient)?;
    restore(&mut pieces, &expressions);
    for piece in &mut pieces {
        if let Piece::Word(word) = piece {
            word.assigns = line.get(word.span.clone()).is_some_and(|raw| {
                raw.split_once('=').is_some_and(|(name, _)| {
                    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            });
        }
    }
    Ok(pieces)
}

/// Why the shell's words in a line can't be read for sure: a command's output fills them in.
const SUBSTITUTION: &str = "it substitutes a command's output ($( or backticks)";

/// What stands for a `${{ … }}`'s bytes while the shell's words are read: two, so that one
/// expression right after another stays its own.
const FILLS: [char; 2] = ['\u{1}', '\u{2}'];

/// `line` with each `${{ … }}`, which GitHub fills in before the shell reads the line, made as
/// many bytes of one filler, and the expressions in order: their spaces and quotes then split
/// nothing, and every place in the line stays where it was.
fn masked(line: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(line.len());
    let mut expressions = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("${{") {
        let Some(end) = rest[start..].find("}}").map(|end| start + end + 2) else {
            break;
        };
        out.push_str(&rest[..start]);
        let fill = FILLS[expressions.len() % 2];
        out.extend(std::iter::repeat_n(fill, end - start));
        expressions.push(rest[start..end].to_string());
        rest = &rest[end..];
    }
    out.push_str(rest);
    (out, expressions)
}

/// Puts back what [`masked`] took out of the words of `pieces`, in order, and says what each word
/// holds.
fn restore(pieces: &mut [Piece], expressions: &[String]) {
    let mut next = expressions.iter();
    for piece in pieces {
        let Piece::Word(word) = piece else {
            continue;
        };
        let plain: String = word.text.chars().filter(|c| !FILLS.contains(c)).collect();
        word.dollar = plain.contains(['$', '`']);
        word.glob =
            !word.quoted && (plain.contains(['*', '?', '[', '{', '%']) || plain.starts_with('~'));
        if plain.len() == word.text.len() {
            continue;
        }
        let mut text = String::with_capacity(word.text.len());
        let mut chars = word.text.chars().peekable();
        while let Some(c) = chars.next() {
            if !FILLS.contains(&c) {
                text.push(c);
                continue;
            }
            while chars.next_if(|other| *other == c).is_some() {}
            if let Some(expression) = next.next() {
                text.push_str(expression);
                word.expressions.push(expression.clone());
            }
        }
        word.text = text;
    }
}

/// The shell's words and operators in `line`, with each `${{ … }}` masked.
fn lex_masked(line: &str, lenient: bool) -> Result<Vec<Piece>, String> {
    let substitution = || SUBSTITUTION.to_string();
    let unclosed = || "its quotes don't close".to_string();
    let mut pieces = Vec::new();
    let mut word: Option<Word> = None;
    let mut chars = line.char_indices().peekable();
    let end_word = |word: &mut Option<Word>, pieces: &mut Vec<Piece>, at: usize| {
        if let Some(mut w) = word.take() {
            w.span.end = at;
            pieces.push(Piece::Word(w));
        }
    };
    // A substitution's text, after its `$(` or backtick, as a lenient read keeps it.
    let substituted =
        |chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, w: &mut Word, close: char| {
            let mut depth = 1;
            for (_, c) in chars.by_ref() {
                w.text.push(c);
                match c {
                    '(' if close == ')' => depth += 1,
                    c if c == close => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        };
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, c)| *c);
        let op: Option<&'static str> = match (c, next) {
            ('&', Some('&')) => Some("&&"),
            ('|', Some('|')) => Some("||"),
            ('&', Some('>')) | ('>' | '<', _) => Some(">"),
            ('&', _) => Some("&"),
            ('|', _) => Some("|"),
            (';', _) => Some(";"),
            ('(' | ')', _) => Some("("),
            ('\n' | '\r', _) => Some("\n"),
            _ => None,
        };
        if let Some(op) = op {
            end_word(&mut word, &mut pieces, at);
            if matches!(op, "&&" | "||") {
                chars.next();
            }
            pieces.push(Piece::Op(op));
            continue;
        }
        if c == ' ' || c == '\t' {
            end_word(&mut word, &mut pieces, at);
            continue;
        }
        // A line that goes on on the next: the shell reads neither character.
        if c == '\\' && matches!(next, Some('\n' | '\r')) {
            end_word(&mut word, &mut pieces, at);
            if chars.next().is_some_and(|(_, c)| c == '\r') {
                chars.next_if(|(_, c)| *c == '\n');
            }
            continue;
        }
        if c == '#' && word.is_none() {
            // A comment, to the end of its line.
            while chars.next_if(|(_, c)| *c != '\n').is_some() {}
            continue;
        }
        let w = word.get_or_insert_with(|| Word {
            text: String::new(),
            span: at..at,
            quoted: false,
            expressions: Vec::new(),
            dollar: false,
            glob: false,
            assigns: false,
        });
        match c {
            '\'' => {
                w.quoted = true;
                loop {
                    match chars.next() {
                        Some((_, '\'')) => break,
                        Some((_, c)) => w.text.push(c),
                        None if lenient => break,
                        None => return Err(unclosed()),
                    }
                }
            }
            '"' => {
                w.quoted = true;
                loop {
                    match chars.next() {
                        Some((_, '"')) => break,
                        Some((_, '\\')) => match chars.next() {
                            Some((_, c)) => w.text.push(c),
                            None if lenient => break,
                            None => return Err(unclosed()),
                        },
                        Some((_, '`')) if lenient => {
                            w.text.push('`');
                            substituted(&mut chars, w, '`');
                        }
                        Some((_, '$')) if lenient && chars.peek().map(|(_, c)| *c) == Some('(') => {
                            chars.next();
                            w.text.push_str("$(");
                            substituted(&mut chars, w, ')');
                        }
                        Some((_, '`')) => return Err(substitution()),
                        Some((_, '$')) if chars.peek().map(|(_, c)| *c) == Some('(') => {
                            return Err(substitution());
                        }
                        Some((_, c)) => w.text.push(c),
                        None if lenient => break,
                        None => return Err(unclosed()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some((_, c)) => w.text.push(c),
                None => w.text.push('\\'),
            },
            '`' if lenient => {
                w.text.push('`');
                substituted(&mut chars, w, '`');
            }
            '$' if lenient && next == Some('(') => {
                chars.next();
                w.text.push_str("$(");
                substituted(&mut chars, w, ')');
            }
            '`' => return Err(substitution()),
            '$' if next == Some('(') => return Err(substitution()),
            c => w.text.push(c),
        }
    }
    end_word(&mut word, &mut pieces, line.len());
    Ok(pieces)
}

/// Why an operator other than `&&` leaves a script as it is.
fn operator(op: &str) -> String {
    match op {
        "||" => "it uses ||".into(),
        "|" => "it pipes one command into another (|)".into(),
        ";" => "it runs commands one after another with ;".into(),
        "&" => "it runs a command in the background (&)".into(),
        ">" => "it redirects input or output (> or <)".into(),
        "(" => "it uses a subshell (parentheses)".into(),
        _ => "it has several lines".into(),
    }
}

/// `NAME=value`, a variable a command line sets for the command after it.
pub fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Shell builtins and keywords: no program to run, so `hushrun run` can't run them.
const BUILTINS: &[&str] = &[
    "cd", "exit", "export", "set", "unset", "source", ".", "eval", "exec", "alias", "return",
    "shift", "trap", "ulimit", "umask", "wait", "read", "if", "for", "while", "until", "case",
    "function", "{", "!", "[[",
];

/// Why a word says the command watches, serves or writes files.
fn watches_or_writes(word: &str) -> Option<String> {
    let off = |flag: &str| word.strip_prefix(flag).is_some_and(|v| v == "=false");
    let watch = matches!(
        word,
        "--watch" | "--watchAll" | "-w" | "watch" | "dev" | "serve" | "start"
    ) || ((word.starts_with("--watch=") || word.starts_with("--watchAll="))
        && !off("--watch")
        && !off("--watchAll"));
    if watch {
        return Some(format!("it watches or serves ({word})"));
    }
    let writes = matches!(word, "--fix" | "--write")
        || word.starts_with("--fix=")
        || word.starts_with("--write=");
    writes.then(|| format!("it writes files ({word})"))
}

/// Test runners that watch on a terminal unless told not to.
fn watches_by_default(words: &[&Word]) -> Option<String> {
    let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let once = |flags: &[&str]| text.iter().any(|w| flags.contains(w));
    match text.as_slice() {
        ["react-scripts", "test", ..] if !once(&["--watchAll=false", "--watch=false"]) => {
            Some("react-scripts test watches on a terminal: add --watchAll=false to wrap it".into())
        }
        ["ng", "test", ..] if !once(&["--watch=false", "--no-watch"]) => {
            Some("ng test watches on a terminal: add --watch=false to wrap it".into())
        }
        _ => None,
    }
}

/// Whether a command is the package manager's own, left as it is: it runs another script, which
/// init wraps on its own, or installs packages. Tool runs (`npm exec`, `pnpm dlx`, `bun x`) and
/// `bun test`, bun's own test runner, are commands like any other.
fn package_manager(words: &[&Word]) -> bool {
    let first = words[0].text.as_str();
    let second = words.get(1).map(|w| w.text.as_str());
    match first {
        "npm" => !matches!(second, Some("exec" | "x")),
        "pnpm" => !matches!(second, Some("exec" | "dlx")),
        "yarn" => !matches!(second, Some("exec" | "dlx")),
        "bun" => !matches!(second, Some("test" | "x")),
        _ => false,
    }
}

/// `vitest` without a subcommand, which watches on a terminal: init makes it `vitest run`.
fn bare_vitest<'a>(words: &[&'a Word]) -> Option<&'a Word> {
    let at = match words[0].text.as_str() {
        "vitest" => 0,
        "npx" | "bunx" => words
            .iter()
            .position(|w| !w.text.starts_with('-') && w.text != words[0].text)?,
        _ => return None,
    };
    let vitest = words.get(at).filter(|w| w.text == "vitest")?;
    let rest = &words[at + 1..];
    let subcommand = rest.first().is_some_and(|w| {
        matches!(
            w.text.as_str(),
            "run" | "watch" | "dev" | "related" | "bench" | "list" | "init" | "typecheck"
        )
    });
    (!subcommand && !rest.iter().any(|w| w.text == "--run")).then_some(*vitest)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-14
    #[test]
    fn a_written_name_is_nothing_an_option_takes() {
        let names = |line| written_names(line);
        assert_eq!(names("hushrun run test -- x"), ["test"]);
        assert_eq!(
            names("npx hushrun run t-${{ strategy.job-index }} -- x"),
            ["t-${{strategy.job-index}}"]
        );
        assert_eq!(names("hushrun run --scope web test -- x"), ["test"]);
        assert_eq!(names("hushrun run test --scope web -- x"), ["test"]);
        assert!(names("hushrun run --scope web -- x").is_empty());
        assert!(names("hushrun run --rev abc -- x").is_empty());
        assert!(names("hushrun run --digest sha256:1 -- x").is_empty());
        assert!(names("hushrun run --stage test -- x").is_empty());
        assert!(names("hushrun run test --stage=web -- x").is_empty());
        assert!(names("npm test").is_empty());
    }

    fn wrap(command: &str) -> String {
        match decide(command) {
            Decision::Wrap { command, .. } => command,
            other => panic!("{command}: {other:?}"),
        }
    }

    fn reason(command: &str) -> String {
        match decide(command) {
            Decision::Leave(reason) => reason,
            other => panic!("{command}: {other:?}"),
        }
    }

    // covers: C-15
    #[test]
    fn a_simple_command_gets_hushrun_run_in_front() {
        assert_eq!(wrap("eslint ."), "hushrun run -- eslint .");
        assert_eq!(wrap("tsc --noEmit"), "hushrun run -- tsc --noEmit");
        assert_eq!(
            wrap("eslint \"src/**/*.{ts,tsx}\" --max-warnings 0"),
            "hushrun run -- eslint \"src/**/*.{ts,tsx}\" --max-warnings 0"
        );
        assert_eq!(wrap("npx eslint ."), "hushrun run -- npx eslint .");
        assert_eq!(
            wrap("cross-env NODE_ENV=test jest"),
            "hushrun run -- cross-env NODE_ENV=test jest"
        );
        assert_eq!(wrap("bun test"), "hushrun run -- bun test");
        assert_eq!(
            wrap("pnpm exec eslint ."),
            "hushrun run -- pnpm exec eslint ."
        );
        assert_eq!(wrap("  jest --ci"), "  hushrun run -- jest --ci");
    }

    // covers: C-15
    #[test]
    fn a_chain_wraps_each_command_except_other_scripts() {
        assert_eq!(
            wrap("vitest run && playwright test"),
            "hushrun run -- vitest run && hushrun run -- playwright test"
        );
        assert_eq!(
            wrap("npm run build && vitest run"),
            "npm run build && hushrun run -- vitest run"
        );
        assert_eq!(
            wrap("pnpm lint&&tsc -b"),
            "pnpm lint&&hushrun run -- tsc -b"
        );
        assert_eq!(
            wrap("yarn install --frozen-lockfile && jest"),
            "yarn install --frozen-lockfile && hushrun run -- jest"
        );
        for only_scripts in [
            "npm run lint && npm run test",
            "pnpm lint && pnpm typecheck",
            "yarn run lint",
            "bun run lint",
            "npm test",
        ] {
            assert_eq!(
                reason(only_scripts),
                "it only runs other scripts or the package manager",
                "{only_scripts}"
            );
        }
    }

    // covers: C-15
    #[test]
    fn what_a_shell_does_beyond_and_leaves_a_script_with_its_reason() {
        for (command, expected) in [
            ("jest || true", "it uses ||"),
            ("tsc; eslint .", "it runs commands one after another with ;"),
            (
                "eslint . | tee lint.txt",
                "it pipes one command into another (|)",
            ),
            (
                "vitest run > out.txt",
                "it redirects input or output (> or <)",
            ),
            ("vitest run 2>&1", "it redirects input or output (> or <)"),
            ("jest < input", "it redirects input or output (> or <)"),
            (
                "eslint $(git diff --name-only)",
                "it substitutes a command's output ($( or backticks)",
            ),
            (
                "eslint `cat files`",
                "it substitutes a command's output ($( or backticks)",
            ),
            (
                "eslint \"$(ls)\"",
                "it substitutes a command's output ($( or backticks)",
            ),
            ("NODE_ENV=test jest", "it sets NODE_ENV=test first"),
            ("tsc && CI=1 vitest run", "it sets CI=1 first"),
            ("server & jest", "it runs a command in the background (&)"),
            ("(cd web && jest)", "it uses a subshell (parentheses)"),
            ("cd web && jest", "it uses the shell's cd"),
            (
                "echo \"Error: no test specified\" && exit 1",
                "it is npm's placeholder",
            ),
            ("eslint 'src", "its quotes don't close"),
            ("vitest run &&", "a command of its && chain is empty"),
            ("   ", "it is empty"),
        ] {
            assert_eq!(reason(command), expected, "{command}");
        }
        // Quoted, the same characters are arguments.
        assert_eq!(
            wrap("eslint --rule 'a: [x || y]' \"b; c\""),
            "hushrun run -- eslint --rule 'a: [x || y]' \"b; c\""
        );
        assert_eq!(wrap("jest A=b"), "hushrun run -- jest A=b");
    }

    // covers: C-15
    #[test]
    fn a_bare_vitest_becomes_vitest_run() {
        assert_eq!(
            decide("vitest"),
            Decision::Wrap {
                command: "hushrun run -- vitest run".into(),
                vitest_run: true
            }
        );
        assert_eq!(
            wrap("vitest --coverage"),
            "hushrun run -- vitest run --coverage"
        );
        assert_eq!(wrap("npx vitest"), "hushrun run -- npx vitest run");
        assert_eq!(
            wrap("tsc && vitest src"),
            "hushrun run -- tsc && hushrun run -- vitest run src"
        );
        assert_eq!(
            decide("vitest run"),
            Decision::Wrap {
                command: "hushrun run -- vitest run".into(),
                vitest_run: false
            }
        );
        assert_eq!(wrap("vitest --run"), "hushrun run -- vitest --run");
        assert_eq!(
            wrap("vitest related a.ts"),
            "hushrun run -- vitest related a.ts"
        );
    }

    // covers: C-15
    #[test]
    fn scripts_already_wrapped_watching_serving_or_writing_are_left_out() {
        assert_eq!(decide("hushrun run -- vitest run"), Decision::Wrapped);
        assert_eq!(decide("hushrun run unit -- jest"), Decision::Wrapped);
        for (command, expected) in [
            ("vitest --watch", "it watches or serves (--watch)"),
            ("jest --watchAll", "it watches or serves (--watchAll)"),
            ("tsc -w", "it watches or serves (-w)"),
            ("vitest watch", "it watches or serves (watch)"),
            ("vitest dev", "it watches or serves (dev)"),
            ("npm run dev", "it watches or serves (dev)"),
            (
                "start-server-and-test start http://localhost:3000 cy:run",
                "it watches or serves (start)",
            ),
            ("vite serve", "it watches or serves (serve)"),
            ("eslint . --fix", "it writes files (--fix)"),
            ("prettier --write .", "it writes files (--write)"),
            (
                "react-scripts test",
                "react-scripts test watches on a terminal: add --watchAll=false to wrap it",
            ),
            (
                "ng test",
                "ng test watches on a terminal: add --watch=false to wrap it",
            ),
        ] {
            assert_eq!(reason(command), expected, "{command}");
        }
        assert_eq!(
            wrap("jest --watch=false"),
            "hushrun run -- jest --watch=false"
        );
        assert_eq!(
            wrap("react-scripts test --watchAll=false"),
            "hushrun run -- react-scripts test --watchAll=false"
        );
        assert_eq!(
            wrap("eslint --fix-dry-run ."),
            "hushrun run -- eslint --fix-dry-run ."
        );
    }

    #[test]
    fn checks_are_named_and_ordered() {
        assert_eq!(kind("lint"), Some(Kind::Lint));
        assert_eq!(kind("format:check"), Some(Kind::Lint));
        assert_eq!(kind("tsc"), Some(Kind::Typecheck));
        assert_eq!(kind("test:unit"), Some(Kind::Test));
        assert_eq!(kind("build"), None);
        assert!(Kind::Lint < Kind::Typecheck && Kind::Typecheck < Kind::Test);
    }

    // covers: C-22
    #[test]
    fn a_run_says_its_scope_and_its_name_in_any_order() {
        let run = |line: &str| runs(line).unwrap().remove(0);
        assert_eq!(run("hushrun run unit -- jest").scope, None);
        let scoped = run("hushrun run --scope web unit -- jest --ci");
        assert_eq!(scoped.scope.as_deref(), Some("web"));
        assert_eq!(scoped.name.as_deref(), Some("unit"));
        assert_eq!(scoped.command, ["jest", "--ci"]);
        let last = run("hushrun run unit --scope=web -- jest");
        assert_eq!(last.scope.as_deref(), Some("web"));
        assert_eq!(last.name.as_deref(), Some("unit"));
        // An option that changes what runs says nothing about a key.
        assert_eq!(runs("hushrun run --force unit -- jest"), None);
        assert_eq!(runs("hushrun run a b -- jest"), None);
        assert_eq!(runs("hushrun run --scope -- jest"), None);
        assert_eq!(runs("hushrun run --scope \"$S\" unit -- jest"), None);
        assert_eq!(runs("hushrun run \"$NAME\" -- jest"), None);
        assert_eq!(
            run("hushrun run --scope web unit -- eslint 'a b'").line("hushrun"),
            "hushrun run --scope web unit -- eslint 'a b'"
        );
    }

    // covers: C-22
    #[test]
    fn a_ci_step_says_where_its_runs_start() {
        let lines = |text: &str| line_runs(text);
        let found = lines("cargo fetch\nhushrun run test -- cargo test --locked\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].moved.as_deref(), Some(""));
        assert_eq!(found[0].run.as_ref().unwrap().command[1], "test");
        // Whatever a shell does around it.
        let found = lines("hushrun run lint -- cargo clippy | tee out.log");
        assert_eq!(found[0].run.as_ref().unwrap().name.as_deref(), Some("lint"));
        let found = lines("RUSTFLAGS=x hushrun run lint -- cargo clippy");
        assert!(found[0].run.is_ok());
        let found = lines("hushrun run lint -- cargo clippy \\\n  --all-targets\n");
        assert_eq!(
            found[0].run.as_ref().unwrap().command,
            ["cargo", "clippy", "--all-targets"]
        );
        // `cd` moves what follows, when hushrun can follow it.
        let found = lines("cd back\nhushrun run t -- cargo test\ncd ../ui && hushrun run t -- x");
        assert_eq!(found[0].moved.as_deref(), Some("back"));
        assert_eq!(found[1].moved.as_deref(), Some("ui"));
        for elsewhere in ["cd \"$DIR\"", "cd /tmp", "cd ..", "(cd a)"] {
            let found = lines(&format!("{elsewhere}\nhushrun run t -- x"));
            assert_eq!(found[0].moved, None, "{elsewhere}");
        }
        // A key CI fills in can't be looked up.
        let found = lines("hushrun run test-${{ matrix.os }} -- cargo test");
        assert!(found[0].run.is_err(), "{found:?}");
        // [ADR 0050] A command CI fills in has a key only the run knows.
        let found = lines("hushrun run test -- cargo test --target ${{ matrix.target }}");
        assert!(
            found[0].run.as_ref().is_ok_and(|run| run.filled_in),
            "{found:?}"
        );
        let found = lines("hushrun run test -- \"$(which cargo)\" test");
        assert!(
            found[0].run.as_ref().is_ok_and(|run| run.filled_in),
            "{found:?}"
        );
        let found = lines("hushrun run test -- cargo test");
        assert!(
            found[0].run.as_ref().is_ok_and(|run| !run.filled_in),
            "{found:?}"
        );
        assert!(lines("echo hushrun run -- nothing").is_empty());
    }

    // covers: C-14
    #[test]
    fn a_ci_job_says_the_names_of_its_runs_as_written() {
        assert_eq!(
            written_names(
                "npm ci\nhushrun run t-${{ matrix.os }} -- x\nA=1 npx hushrun run --scope web u -- y"
            ),
            ["t-${{matrix.os}}", "u"]
        );
        assert_eq!(
            written_names("hushrun run --rev main \"v-${{ strategy.job-index }}\" -- x"),
            ["v-${{strategy.job-index}}"]
        );
        for none in [
            "hushrun run -- x",
            "hushrun run --stage staging unit -- x",
            "hushrun run unit",
            "echo hushrun run unit -- x",
            "# hushrun run unit -- x",
        ] {
            assert!(written_names(none).is_empty(), "{none}");
        }
    }

    // covers: C-15
    #[test]
    fn a_ci_line_says_what_can_be_wrapped() {
        let mut here = Some(String::new());
        let found = commands("cargo fetch && cargo test --locked", &mut here);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].at, 15);
        assert_eq!(found[1].words, ["cargo", "test", "--locked"]);
        assert_eq!(found[1].leave, None);
        let found = commands("cargo test | tee out.log", &mut here);
        assert_eq!(
            found[0].leave.as_deref(),
            Some("it pipes one command into another (|)")
        );
        // [ADR 0050] Variables set first, and words CI or the shell fills in, are for init to judge.
        // A launcher's option's value is no word of the command's.
        let found = commands("uv run --group dev pytest tests", &mut here);
        assert_eq!(found[0].leave, None, "{found:?}");
        let found = commands("UV_FROZEN=1 uv run --group dev pytest tests", &mut here);
        assert_eq!(found[0].leave, None, "{found:?}");
        let found = commands("npm run dev", &mut here);
        assert_eq!(
            found[0].leave.as_deref(),
            Some("it watches or serves (dev)")
        );
        let found = commands("uv run --frozen ruff check --fix", &mut here);
        assert_eq!(found[0].leave.as_deref(), Some("it writes files (--fix)"));
        let found = commands("RUST_LOG=debug cargo test", &mut here);
        assert_eq!(found[0].leave, None);
        assert!(found[0].prefixed && !found[0].fills_in, "{found:?}");
        // A quoted value sets it all the same; a quoted name is a command's.
        let found = commands("RUSTFLAGS=\"-D warnings\" cargo test", &mut here);
        assert!(found[0].prefixed, "{found:?}");
        assert_eq!(found[0].words, ["RUSTFLAGS=-D warnings", "cargo", "test"]);
        assert!(!commands("\"NAME=a\" cargo test", &mut here)[0].prefixed);
        let found = commands("pytest -q ${{ matrix.args }} \"$EXTRA\"", &mut here);
        assert_eq!(
            found[0].words,
            ["pytest", "-q", "${{ matrix.args }}", "$EXTRA"]
        );
        assert!(found[0].fills_in && !found[0].prefixed, "{found:?}");
        assert_eq!(found[0].leave, None);
        // An expression's spaces and quotes split nothing, and places in the line hold.
        let found = commands("echo ${{ format('a b', 'c') }} && cargo test", &mut here);
        assert_eq!(found[0].words, ["echo", "${{ format('a b', 'c') }}"]);
        assert_eq!(found[1].at, 34);
        // What the shell expands into other words is still left.
        let found = commands("pytest tests/*.py", &mut here);
        assert_eq!(
            found[0].leave.as_deref(),
            Some("the shell makes its words what they are")
        );
        // A command's output, which only init's reading keeps, and leaves.
        assert!(commands("go test $(go list ./...)", &mut here).is_empty());
        let found = init_commands("go mod download && go test $(go list ./...)", &mut here);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].leave, None);
        assert_eq!(found[1].leave.as_deref(), Some(SUBSTITUTED));
        // A pipe that only shows the output, last on its line, stops nothing.
        for line in [
            "xcodebuild test -scheme App | xcpretty",
            "xcodebuild test -scheme App | xcbeautify --quiet",
            "xcodebuild test -scheme App | bundle exec xcpretty",
            "set -o pipefail && xcodebuild test -scheme App | xcpretty",
        ] {
            let found = commands(line, &mut here);
            let check = found
                .iter()
                .find(|found| found.words[0] == "xcodebuild")
                .expect("xcodebuild");
            assert_eq!(check.leave, None, "{line}");
            assert_eq!(
                check.words,
                ["xcodebuild", "test", "-scheme", "App"],
                "{line}"
            );
            assert!(line[check.at..].starts_with("xcodebuild"), "{line}");
        }
        for (line, why) in [
            ("xcodebuild test | xcpretty -r junit", "pipes"),
            ("xcodebuild test | xcpretty --report junit", "pipes"),
            ("xcodebuild test | xcbeautify --report-path out", "pipes"),
            ("xcodebuild test | xcpretty | tee log", "pipes"),
            ("xcodebuild test | tee log | xcpretty", "pipes"),
            ("xcodebuild test | xcpretty > out.log", "pipes"),
            ("xcodebuild test | xcpretty && echo done", "pipes"),
            ("xcodebuild test | xcpretty; exit 1", "pipes"),
            ("xcodebuild test 2>&1 | xcpretty", "redirects"),
        ] {
            let found = commands(line, &mut here);
            assert!(
                found[0].leave.as_deref().is_some_and(|l| l.contains(why)),
                "{line}: {:?}",
                found[0].leave
            );
        }
        let found = commands("go test --target ${{ matrix.x }}", &mut here);
        assert_eq!(found[0].leave, None);
        assert!(found[0].fills_in, "{found:?}");
        let found = commands("cargo fmt --write", &mut here);
        assert_eq!(found[0].leave.as_deref(), Some("it writes files (--write)"));
        let found = commands("cd back && cargo test", &mut here);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].moved.as_deref(), Some("back"));
    }

    // covers: C-15
    #[test]
    fn command_lines_name_the_scripts_they_run() {
        assert_eq!(scripts_run("npm ci && npm test"), ["test"]);
        assert_eq!(scripts_run("npm t"), ["test"]);
        assert_eq!(scripts_run("npm run lint -- --max-warnings 0"), ["lint"]);
        assert_eq!(scripts_run("npm run-script typecheck"), ["typecheck"]);
        assert_eq!(scripts_run("npm run -s lint"), ["lint"]);
        assert_eq!(scripts_run("pnpm lint; pnpm run test"), ["lint", "test"]);
        assert_eq!(scripts_run("yarn lint && yarn run test"), ["lint", "test"]);
        assert_eq!(scripts_run("bun run lint"), ["lint"]);
        assert_eq!(scripts_run("CI=true npm test"), ["test"]);
        // bun test is bun's own runner, not the script.
        assert!(scripts_run("bun test").is_empty());
        // Another package's scripts.
        assert!(scripts_run("cd web && npm test").is_empty());
        assert_eq!(scripts_run("cd . && npm test"), ["test"]);
        assert!(scripts_run("npm --prefix web test").is_empty());
        assert!(scripts_run("pnpm --filter web test").is_empty());
        assert!(scripts_run("pnpm -r test").is_empty());
        assert!(scripts_run("npm run --workspace web test").is_empty());
        assert!(scripts_run("npx vitest run").is_empty());
        // Several lines, as a block runs them: substitutions, comments and quotes that would
        // leave a script out don't hide the commands around them.
        assert_eq!(
            scripts_run(
                "echo \"sha=$(git rev-parse HEAD)\" >> \"$GITHUB_OUTPUT\"\n# then the checks\nnpm run lint\necho `date`; npm test"
            ),
            ["lint", "test"]
        );
        assert!(scripts_run("npm ci\ncd web\nnpm test").is_empty());
        // A run of hushrun's own starts what follows its `--`.
        assert_eq!(scripts_run("hushrun run -- npm test"), ["test"]);
        assert_eq!(scripts_run("npx hushrun run all -- npm run lint"), ["lint"]);
        assert_eq!(
            scripts_run("hushrun run --scope web a -- pnpm test && npm run b"),
            ["test", "b"]
        );
        assert!(scripts_run("hushrun run all -- go test ./...").is_empty());
        assert!(scripts_run("hushrun run all").is_empty());
        // Unless CI fills in its name: each variant has a key of its own.
        let named = "npm run a && hushrun run t-${{ strategy.job-index }} -- npm test";
        assert_eq!(scripts_run(named), ["a"]);
        assert_eq!(scripts_in_named_runs(named), ["test"]);
        let named = "hushrun run t-${{matrix.os}} -- npm run b";
        assert!(scripts_run(named).is_empty());
        assert_eq!(scripts_in_named_runs(named), ["b"]);
        assert!(scripts_in_named_runs("npm test").is_empty());
        assert!(scripts_in_named_runs("hushrun run t -- npm test").is_empty());
    }
}
