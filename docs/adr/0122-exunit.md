# 0122. ExUnit

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-114 to C-117; relies on C-30, C-31, C-32, C-36, C-38, ADR 0068, 0069, 0080

## Context

`hushrun init` has known `mix test` as a whole check for a long time, and no adapter looked at its
test files: an Elixir project ran every test file on every change. ExUnit takes no plugin from the
environment, Mix's own `--stale` keeps its answers in `_build` and decides by itself, and nothing
of the repository should have to change for hushrun to look. Elixir 1.14 on Erlang/OTP 25 was
tried here, a plain project, an umbrella and a module called only by a name made at run time.

## Decision

1. **Loading, through the VM's own variables.** hushrun compiles one Erlang module, embedded in
   the binary, with the `erlc` on COMMAND's `PATH` into
   `<cache dir>/adapters/exunit-<hash>/hushrun_exunit/ebin` (the hash holds the source and that
   `erlc`), names the folder first in `ERL_LIBS`, and adds
   `-eval catch(hushrun_exunit:start()).` to `ELIXIR_ERL_OPTIONS`, each keeping what it had. Every
   Erlang VM of COMMAND starts it before Elixir's CLI; with no `HUSHRUN_UNITS`, or when the module
   can't load (an `erl` older than that `erlc`), it does nothing and the VM goes on. Erlang, not
   Elixir, so that one beam serves every Elixir version, and it is loaded before Mix prunes code
   paths. It starts the `elixir` application, adds itself to the compiler's tracers and appends
   itself to ExUnit's formatters in the `ex_unit` application's environment.
2. **Which COMMANDs**: `mix test` (after `env NAME=value`), from any folder, an umbrella's root
   included. Not offered, said once: `--stale` and `--failed` (they choose from Mix's own earlier
   runs), `--listen-on-stdin`, `--formatter` (it replaces ExUnit's formatters, hushrun's among
   them). `mix do test`, an alias of another name and `iex -S mix test` aren't recognized.
3. **A unit is a test file** by Mix's default rule, from the files alone: a file ending
   `_test.exs` below the `test` folder of a project with a `mix.exs` in the scope. A project whose
   `mix.exs` names `test_paths` or `test_pattern` has no unit by the rule; a file ExUnit ran that
   the rule doesn't name is kept, `not observed` (`test-paths`). Sources are `.ex`, `.exs` and
   `.erl`; every other file is rest, so a template, an `.hrl`, a fixture or `mix.lock` runs every
   test file when it changes.
4. **What its code references, to the end.** When a file's compilation stops, the tracer asks
   Elixir's lexical tracker for its compile, export and runtime references, as `mix test --stale`
   does. At the suite's start the formatter reads every Mix compile manifest of an application
   whose folder is in the repository (the project, an umbrella's apps, path dependencies), each
   module's sources and each source's references and external resources, through
   `Mix.Compilers.Elixir.read_manifest/1`, whatever the manifest's record layout (lists of records
   before 1.15, maps after); a module no manifest names, Erlang's, by its beam's source and its
   imports. A test file read its own file, the closure of what it references, and every file the
   project's configuration is made of: each `mix.exs` from its folder up to the root, `mix.lock`,
   the config folder's `.exs` files and the test helpers that ran. A manifest that reads as empty
   makes every test file of the suite `not observed` (`manifest`).
5. **What ran, by process.** The static closure misses a module called by a name made at run time
   (`Module.concat`, a behaviour's implementation from config). At the suite's start every module of
   the repository is loaded, as `mix test --slowest` has Mix do, and gets a `call_time` counter on
   each exported function; every process is traced for calls (no function has a pattern that sends
   a message) and for spawns, by a tracer that keeps only who started each process. At each test
   module's start and end the counters are read by process and set back to zero. A process is a
   test file's when it called that file's test module, or when its start (ExUnit's function holds
   the test module), its parent's, or its `$callers` or `$ancestors` lead to one: what it called is
   that file's, with the closure of what it references. Any other process's calls (an
   application's server), and files evaluated while tests ran, are read by every test file running
   then or whose start is still queued, and, when none ran, by every test file that ends after. A
   port opened (`System.cmd`, `:os.cmd`) makes its test file, or those, `not observed`
   (`subprocess`).
6. **Leaving out.** The first VM to start with the run's directory claims it (`exunit-claim`). Once
   a test helper has run, it adds `{file, <path>}` for each test file `reuse` lists to ExUnit's
   `exclude`, which `mix test` keeps beside its own (`merge_helper_opts`): ExUnit compiles the file
   and runs none of its tests, nor `setup_all`, and says them excluded. A user's `--include` or
   `--only` wins over it, and the tests that ran are told as run.
7. **Outcomes.** One `hushrun.unit-facts/v0` file per suite (an umbrella runs one per app in one
   VM), failures by unit: a test file failed when a test failed or was invalid (its `setup_all`
   failed), finished tests count passes, failures and skips, and one whose every test was excluded
   by hushrun's filter is `deselected`. A `.started` file from the suite's start to its file left
   behind makes the run record nothing.

Tests one by one aren't offered: ExUnit's filters match one tag at a time, and a test's name is
unique only within its module, so no filter leaves out one test of one file without risking a
test of the same name in another. `unit: test` reuses by test file and says so.

## Consequences

- An Elixir project's `mix test` reruns only the test files that read what changed, a dependency
  bump or a config change running all of them.
- Every module of the repository is loaded before the first test, which `function_exported?/3` on
  a module nothing loaded yet can tell; `@on_load` hooks run then.
- A test that traces processes itself with a tracer of its own (`:dbg`, `:erlang.trace/3`) finds
  them traced by hushrun's, which can fail it: such a check runs without `tests`. Where tracing is
  taken before the suite starts, the counters are one for every process, and every call goes by
  the time windows of item 5.
- What an application's own processes call is shared by the test files running at once: an async
  suite whose tests drive a shared server reads more than it needs, never less.
- A source read as data (`File.read!("lib/x.ex")`) isn't followed; an `.exs` file evaluated at run
  time is (item 5).
- Not run here: Elixir 1.15 and later (manifests as maps; CI runs a current Elixir), Windows.

## Compiled code

`mix compile` shares nothing yet: B-260 to B-269 are kept for it. Mix has no cache protocol to
serve, so a design has to restore `_build/<env>/lib/<dep>` before Mix decides a dependency is
stale (through the same loader, around `Mix.Tasks.Deps.Compile`), key it by the dependency's
`mix.lock` entry, the Elixir and OTP versions, `MIX_ENV` and `MIX_TARGET`, and the project's
compile-time configuration (which Mix compiles dependencies under), keep native code (`elixir_make`,
`rustler`) out or keyed by the C toolchain, and travel as a kind of its own through the relay.

## Alternatives

- **A Mix task or archive** (`mix hushrun.test`): it changes the COMMAND people write, and an
  archive in `MIX_ARCHIVES` hides the person's own (Hex).
- **`mix test --stale`'s manifest**: it is Mix's, written only with `--stale`, and decides by
  timestamps.
- **Global call counters only**: ExUnit sends its events asynchronously, so a formatter's window
  lags the tests; with fast tests every file read what its neighbours ran.
- **Excluding by test name**: a name is unique only within a module (above).
