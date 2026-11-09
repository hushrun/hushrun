# 0037. Any language, from CI's own files; several scopes; inputs outside the repository

- **Status:** accepted; amended by [0050](0050-init-reads-what-ci-runs.md) on 2026-11-05: items
  1, 2, 3 and 6, for composite actions, words CI fills in, variables set in front of a command,
  targets read by their recipe, more runners, and tools with a remote cache; and by
  [0051](0051-a-workspace-package-is-a-scope.md) on 2026-11-05: item 4, for a JavaScript workspace,
  whose packages are scopes
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-11-01
- **Rules:** C-3, C-4, C-8, C-9, C-10, C-11, C-12, C-13, C-14, C-15, C-22, C-23, C-24
- **Amends:** [0023](0023-a-repositorys-backend-and-the-hosted-default.md), whose only home for a
  repository's backend was package.json; [0036](0036-agents-told-before-they-push.md), whose
  `hushrun ready` read package.json's scripts at the root, in one scope

## Context

We ran hushrun on its own two repositories, and it couldn't set either up. Neither has a
package.json at its root: `cli` is Rust, and `cloud` is a Rust server beside three web apps. The
same is true of any Rust, Go, Python, Java, Kotlin, Ruby, PHP, .NET, Elixir or Swift repository.
Four things stood in the way:

- `hushrun init` found checks among package.json's scripts, and wrapped nothing else.
- `hushrun ready` gave up on a repository with several scopes, so the hook could never refuse a
  push there, and it knew no checks without package.json or stages.
- A repository with no package.json had nowhere to commit its org's URL, so every machine and CI
  job needed `HUSHRUN_BACKEND`.
- A check could only be about the repository's own files. A server that builds against `../cli`
  through path dependencies its lockfile doesn't pin, so a change there could skip a check that
  should run.

Replaying `cloud`'s 19 real pushes showed where the saving is: scopes would have saved 44% of its
runner minutes, with CI's and the root's files in each, against almost nothing from matching the
whole repository. So several scopes aren't an edge case: they are what makes hushrun worth having
in a monorepo.

## Decision

1. **CI's own files are the list of checks.** GitHub's workflows and GitLab's pipeline already
   name a repository's checks in any language, so there is no second list to keep in step, and
   nothing to learn per language beyond what a command is. init reads them, and so does `ready`.

2. **Recognition is one table** (`cmd/setup/checks.rs`): a row is a program, its words, and an
   example the tests wrap through a workflow. Adding a language is adding a row. A command the
   table doesn't know stays as it is and says nothing; a command it knows can't skip, such as
   `cargo deny`, whose answer changes with an advisory database, stays with the reason.

3. **init wraps in place** (C-15), as it edits package.json: the file keeps every other byte. It
   adds what the job needs: hushrun's install and, on GitHub, an ID token. A check's name is the
   table's; two jobs of one command become two checks, since they may run on two systems, which a
   key doesn't tell apart (C-9). What can't skip safely stays, with the reason: a matrix of several
   variants, a command that writes a report a later step may read, a line with `|` or `;`.

4. **A repository of projects gets a scope each, when that is sure.** A scope that leaves out
   something a check reads skips a check that should run, which is the one mistake hushrun must
   not make. And a project reads more than its manifest says: a project can have a
   test that opens `../web/config.json`, the one test that guards that file. So a split
   rests on three things:

   - **The root's files are in every scope**, dotfiles included, as `*`, which stops at a `/`: a
     project's tools read configs there that no manifest names, such as a `tsconfig` a project
     `extends`, a shared lint or format config, `rust-toolchain.toml` or `Directory.Build.props`.
     The root's prose is left out (`*.md`, `LICENSE*`, `NOTICE*`), which stops at a `/` too: a
     changelog that every commit touches would run every check. A project's own docs still count,
     and a root file a project names stays in. Every scope also holds `.github/workflows/**`,
     `.github/actions/**` and `.cargo/**`.
   - **What its manifests name:** Cargo's, npm's, Python's and Go's, and Terraform's modules.
   - **What its files name:** a lexical scan of the text files git tracks, for paths that start
     with `../`, each resolved from its file and from the project's directory, which is how a
     test's `join("../web/x")` reads. In quotes, a path can hold spaces. Without quotes, as in
     `cat ../lib/gen.txt`, `context: ../lib` or `$(MAKE) -C ../lib`, it ends at whitespace or a
     delimiter. It starts where nothing before it is more of a path, or after what stands for a
     directory, as in `$(dirname "$0")/../lib`. Only what exists counts. It finds `extends` and
     `references` the same way. A hit joins the scope as that file or that directory, or becomes
     `external` (item 7), exactly as a manifest's does, and what it reaches is read in turn.
   - **Where its links lead:** a link git tracks names where it leads, since the digest holds only
     the link's text: a change under what `web/shared` points at has to run `web`'s checks. An
     absolute link, or one that leads out of the project to nothing git tracks, keeps one scope.

   A hit can be wrong, which widens a scope and costs a run: the safe direction. Three exceptions
   keep a wrong hit from costing more than that. A `.md` file isn't scanned: a README's
   `` `../admin` `` reads nothing, and would join the two projects for good. A path without quotes
   in a comment doesn't count: `cloud`'s usage notes say `cd ../../cli`, which would tie `landing`
   to that checkout, and only what is surely a comment is left out. And a path that works from its
   own file isn't taken for a checkout beside the repository from the project's directory: an
     `external` that isn't one stops the project skipping at all.

   A workspace at the root, a manifest hushrun can't read yet, a project that names the whole
   repository, or one too big to read keeps one scope, and the plan says why: that costs a run,
   never a wrong skip. Scopes in an existing `hushrun.yaml` are kept, and one with stages gets none
   added: evidence names its scopes.

   **What this can't see:** a path built at run time, such as
   `path.join(__dirname, "..", "..", "landing")`; and a file over 1 MiB, which isn't read. The plan
   says so whenever it splits, and the fix is a line in the scope's `include`.

5. **`ready` looks a check up by the key `hushrun run` would use** (C-22): its `--scope`, its
   directory, and what the scope declares outside the repository. Several scopes get real states.
   A run with no `--scope` among several would run without skipping and record nothing (C-14), so
   asking for it would never end: it stays with CI, as ADR 0036's invariant says it must.

6. **What a job needs that this machine lacks stays with CI.** Services, a container, a secret, a
   condition, another operating system, a tool that isn't installed, a workflow only some paths
   start, or a key CI fills in with `${{ }}`: nothing here can pass those for sure. A workflow CI wouldn't start for this push says
   nothing at all, so an agent isn't sent to run a nightly job. Only a known gap stops a push.

7. **A scope declares what it builds against outside the repository**, under `external` (C-4): a
   path relative to the repository's root, the top of a git checkout, with `include` and
   `exclude`. It is digested by §4's rules from its worktree, dirty files included, whatever
   revision the repository is at: that is what a build here reads. Its digest joins the run's key
   only when a scope declares one, so every key that exists stays as it is. A path that's missing,
   or isn't the top of a checkout, is a doubt: the command runs without skipping and says why
   (C-14).

8. **`external` and stages don't go together, for now.** Evidence (E-4) is about a scope's digest,
   and an artifact promotes that digest from stage to stage: what a sibling held isn't part of what
   is promoted, and a plan in CI couldn't reproduce it from a laptop's sibling. Counting it would
   need a new event field and new rules for promotion. Silently ignoring it would be a wrong skip
   with a signature on it. So `hushrun.yaml` refuses the combination (C-8), which says what to do.

9. **`backend:` in `hushrun.yaml` is the neutral home** for what package.json's `hushrun.backend`
   does (C-3, C-11), which comes first when both are there. It is a file in the repository, so it
   gets exactly the tokens package.json's gets (C-13): never `HUSHRUN_TOKEN`, and an agent token
   only on hushrun's host. A repository with no package.json needs no variable.

10. **A lookup can ask what a repository's CI would find** (C-12). "CI reuses only CI's results"
    is only ever told to that repository's CI, so `ready` on a laptop asked for runs CI wouldn't
    reuse, and called a laptop's pass ready. `?as=ci&repository=<id>` answers as to that CI. `ready`
    then asks for nothing here, and a pass counts only if CI recorded it. A server from before the
    query ignores it, and `ready` behaves as it did: an unneeded run, never a trap. `hushrun run`
    doesn't ask, since a laptop may still reuse its own passes.

11. **Clones without package.json are told about the hook.** git doesn't clone hooks, and a package
    manager's `prepare` was what installed it. Nothing else runs on a clone, so init says each clone
    runs `hushrun hook install` once, and so do `AGENTS.md` and the skill. \[ADR 0043\] The first
    `hushrun ready` or `hushrun run` in a clone now installs it instead.

## Alternatives

- **A `hushrun.yaml` list of checks for every language.** A second list, which drifts from CI, and
  a config file before the first skip. ADR 0023's point was that there is none.
- **Reading manifests instead of CI's files**, such as `Cargo.toml` meaning `cargo test`. It guesses
  what a repository runs. CI's files say it, flags and all, and the key depends on those flags.
- **Parsing workflows with a YAML library and writing them back.** It would lose comments and
  layout. They are read and edited as lines, as they already were.
- **A check name holding the sibling's commit**, the workaround we first reached for. It misses a
  dirty sibling, so a laptop's pass could stand in for code nobody committed.
- **Putting the sibling's digest in the scope's own digest.** It would change what `digest` means
  to everything built on it, promotion first.
- **Per-project scopes by directory alone, or by manifests alone.** Simpler, and wrong for a
  project that reads a neighbour: both would skip a check that should run. Manifests alone was this
  decision's first version, and review found four wrong skips in it, one of them ours.
- **Parsing each language's imports.** It would still miss a test that opens a file, and it is a
  parser per language to keep. A `../` reads the same in all of them, and being wrong widens a
  scope.

## Consequences

- A repository in any language gets a working setup from `npx hushrun init`, and `ready` and the
  hook work in it, several scopes included.
- A `hushrun.yaml` with `backend` or `external` doesn't parse on hushrun before 0.4.0, which then
  runs every check without skipping. CI installs the newest release by default (ADR 0035).
- `ready` lists checks from CI's files in a repository that has them and package.json scripts, so a
  repository set up before may see more checks. Only checks a command can pass count against it.
- A monorepo with one workspace at its root stays in one scope until someone writes its scopes by
  hand. init doesn't know which of its files a project reads.
- A scope can still miss what no text says: a path built at run time. Those are a person's to add,
  and the plan and the guide say so.
- A root file that isn't prose runs every project's checks when it changes, which is what `*`
  costs. On `cloud`'s 19 pushes that cost nothing: 2 touched such a file, and both ran everything
  anyway.
- init can't tell that a test reaches the outside world. A check like that needs taking out of
  `hushrun run` by hand: nothing in a command says it.
- The results protocol gains a query (C-12): hushrun cloud should deploy it before people rely on
  `ready` in a repository with that setting on.
