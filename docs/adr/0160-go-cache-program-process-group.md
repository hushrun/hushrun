# 0160. Go's cache program leads a process group of its own

- **Status:** proposed; built
- **Date:** 2026-11-27
- **Rules:** B-40 changed; relies on B-39 and B-26

## Context

`build_go a_frozen_relay_costs_a_run_one_wait_not_one_per_go_command` failed now and then on
CI's macOS job, on pull requests that touched neither Go nor the relay. The test stops `hushrun
run` with SIGSTOP to freeze its relay, then expects three `go build` commands to finish: the first
after one 30 s wait for the relay, the others without asking it.

There were two failure modes:

- On main's code, the test process itself sometimes died of SIGHUP at about 65 s.
- Given a process group of its own (closed #83), the test failed on every macOS run. The Go
  builds never finished, and it reported "go hung with a frozen relay".

The relay client and the Go cache program were not the cause. A diagnostic in the test recorded
the process group's timeline on macOS:

- The cache program gave up on the frozen relay after one wait and wrote `go-relay-down` (B-40),
  as designed.
- Just as a `go build` ended, the stopped `hushrun run` died of SIGHUP. Its shell got SIGHUP and
  was reparented to launchd, and a `go build` that was still running exited with 129. In the
  failing run that had its own group, everything in the group was gone before the test looked.
- In the same freeze with only `sleep` in COMMAND, nothing was hung up. Likewise with `go build`
  but no cache program.

Go never waits for its cache program (`cmd/go/internal/cache/prog.go`). After `close`, `Close`
closes the program's stdin and reads its stdout to the end, which comes as the program exits, and
nothing calls `Wait`. So the `go` command often exits while its program is still exiting, and the
system makes launchd (init on Linux) the program's parent.

On macOS, the end of a process under launchd that is still in the `go` command's group counts as
that group losing its last link to its session. That group is COMMAND's, and the `hushrun run`
that started COMMAND is in it. A group that is orphaned while one of its processes is stopped
gets SIGHUP and then SIGCONT, to every process in it. Linux works out orphaned groups from the
processes' actual parents, and never failed.

On main, the test process shared that group, so it was hung up too: the SIGHUP at 65 s. On the
runs where the program happened to finish before its `go` command, nothing was hung up, and the
test passed.

This is not only a test's problem. A `hushrun run` that is stopped while COMMAND runs `go`
commands gets the same hang-up, and so do COMMAND and the `go` commands still to come. Examples:
a job a terminal stopped for writing to it in the background, or one held in a debugger.

## Decision

1. **The program leaves the `go` command's process group before anything else.** It calls
   `setpgid(0, 0)` first thing in its `main`, before it reads a setting or writes a line. Its end,
   under whatever parent, then touches only its own group, which holds no stopped process.
   `serve`, which tests call in-process, doesn't do this: only the binary started by the
   program's name does.
2. **Nothing reaches the program through a group, and nothing needs to.** It ends at `close`, or
   when its input ends because Go is gone. A terminal's Ctrl-C, which goes to the foreground
   group, no longer reaches it directly. The `go` command it serves ends on that Ctrl-C, and the
   program then sees its input end. The program opens no terminal, and none of its children
   inherit its stdout. It writes nothing on stderr (B-40), so a background group's terminal
   signals never stop it.
3. **The frozen-relay test** gives its `hushrun run` a process group of its own. A hang-up then
   can't end the test, and the SIGCONT that comes with it can't unfreeze hushrun and hide the
   hang. The test stops hushrun once COMMAND has started, not after a fixed 1.5 s: stopped before
   it starts COMMAND, hushrun runs nothing at all.

## Consequences

- A stopped `hushrun run` stays stopped while its `go` commands come and go, on macOS as on
  Linux, until something continues it. A frozen relay costs a run one 30 s wait on macOS too.
- After a Ctrl-C, the program can outlive its `go` command by up to the watchdog's 40 s (B-40),
  while a handler waits on the relay. Its input has already ended, so it answers nothing. It
  only lets its handlers finish what they write in `GOCACHE`, as it did when Go closed its
  stdin.
- `the_cache_program_runs_in_a_process_group_of_its_own` starts the binary by the program's name
  and checks its group. It fails if the call goes.
- The C and C++ launcher and the `rustc` wrapper need no such change. `make`, `ninja` and cargo
  wait for the compilers they start, so nothing they start is left to launchd while it runs.

## Alternatives considered

- **Fix the test alone**, either by keeping it out of hushrun's group or by not freezing hushrun
  at all. The first only made the hang certain: nothing hung up the group any more, and nothing
  continued hushrun. The second leaves the product's hang-up in front of every stopped run.
- **Make the program exit before Go reads the end of its stdout.** A process's descriptors close
  as it exits, before it is gone, so Go can always see the end of stdout first. Closing stdout
  early and lingering makes the window wider, not narrower.
- **A new session (`setsid`)** in place of a new group. That also detaches the program from the
  terminal's session, which nothing needs. A group is the smallest change that keeps the
  program's end out of COMMAND's group.
- **Have `hushrun run` reap or adopt the program** (a subreaper). macOS has no subreaper, and
  hushrun is the stopped process here, so it couldn't reap anything anyway.
