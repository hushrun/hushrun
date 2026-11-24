# 0116. PHPUnit and Pest

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-90, C-91, C-92, C-93; relies on C-30, C-31, C-32, C-34, C-38, C-39, ADR 0068, 0069,
  0071, 0080, 0085, RFC 0004

## Context

`hushrun init` already knew `vendor/bin/phpunit`, `vendor/bin/pest` and `composer test` as whole
checks, and no adapter observed them: a PHP check reran every test whenever any file changed.
PHPUnit 10 rebuilt its runner on an event system (`PHPUnit\Event`) that 11 and 12 keep, and Pest 3
and 4 run on PHPUnit 11 and 12. PHPUnit takes no plugin from the environment, and its extensions
are registered in `phpunit.xml`, which hushrun doesn't edit (ADR 0068's rule: no file of the
repository changes). PHPUnit 9 and older have no events, and are left out.

PHP has no compile step ahead of a run: there are no build blocks for it (B-200 to B-209 stay
unused), and this decision is about tests only.

## Decision

1. **Loading through PHP's ini scan directory.** hushrun writes two PHP files and an ini file to
   `<cache dir>/adapters/phpunit-<hash>/`, the ini file setting `auto_prepend_file`, and adds that
   directory to `PHP_INI_SCAN_DIR` after what it had (an empty first entry keeps PHP's own). Every
   PHP process of COMMAND, Composer's included, runs the first file first; it does nothing unless
   the process is PHPUnit's or Pest's own program, by `argv[0]`, and is the first to claim the
   run's directory. That process gets `--bootstrap <hushrun's bootstrap>`; the bootstrap registers
   one tracer of PHPUnit's events (`Facade::registerTracer`, before PHPUnit seals the facade) and
   then includes the bootstrap the program would have loaded, COMMAND's or the configuration's.
   A prepend file PHP's own configuration names is asked of `php -r` before COMMAND and run first.
   - A **tracer**, not one subscriber per event: one class, in 10, 11 and 12 alike, reads every
     event by its class name, so an event a version lacks is never named in a declaration.
   - PHPUnit 10 has no `--extension`; PHPUnit 11 and 12 have it, but the class must be loadable
     before the bootstrap anyway. The bootstrap is the one place all three load hushrun's code
     before the facade is sealed, and including the user's bootstrap from it is what PHPUnit does
     with a bootstrap (`include_once`, the configuration already applied).
2. **Which COMMANDs.** `phpunit` or `pest` by name or path, `php [options] <either>`, and a
   Composer script that runs exactly one of them (`composer test`, which `init` proposes, is the
   common case). A script hushrun can't read (`make test`, `php artisan test`) gets the default
   adapter, which does nothing. Not offered: `php -n`, which reads no ini file; a `-d
   auto_prepend_file`; what lists or configures instead of running; Pest's parallel runs, whose
   workers are processes paratest starts; watching; choosing tests from git; and other majors
   than PHPUnit 10, 11, 12 and Pest 3, 4.
3. **Units and tests.** A unit is a test file, from the runner's own list (`TestSuite\Loaded`,
   before filters), as Jest's (ADR 0085). A test is a test method with its data set, as PHPUnit
   names it, its id prefixed with its class so that a file of several classes stays apart; for
   Pest, the method's name without `__pest_evaluable_`, since Pest's class name is made from the
   path.
4. **What a test file read: everything loaded before its end.** PHP loads a class once per process
   and runs no code to find it again: a test that uses a class an earlier test loaded leaves no
   trace. Without a coverage driver there is no per-call record, and none is required here. So a
   test file read every file the process had included (`get_included_files()`) or opened by the
   end of its last test. That is sound, can only read more, and it improves as reuse works: a run
   that leaves test files out loads less before the ones it runs, so they read less, and the next
   change runs fewer.
   - **Opens** are followed by replacing PHP's `file` stream wrapper for the process: every open,
     include and stat goes through it to the native wrapper, and a non-include open is recorded.
     It matters because sources are `.php`, and PHP files are read as data, not only included: a
     Blade template, a static analyser's fixture, a code generator's input. A wrapper that another
     replaced during the run (dg/bypass-finals does this) makes every unit `not observed`.
   - **Other test files** are taken out: PHPUnit includes every test file of the suite before the
     first test runs, so without this one test file's change would run every test file. A test
     file is still read where the unit's class inherits from one of its classes, or the unit's
     code names one of them (by token: a bare name, a qualified name, or a string that is a class
     name), which covers external data providers, `#[Depends]` across classes and shared helpers
     on a test class.
   - Facts carry it once a run: the ordered list of every path read (`settings.sequence`) and
     each unit's count of it (`readTo`), so the file grows with the files read, not with them
     times the test files (890 KB to 234 KB on the sample below).
5. **Outcomes**, from events, with what PHPUnit fails a run on: a test's own warnings, notices,
   deprecations and risky tests fail it only where the configuration's `failOn…` says they fail
   the run, so an ordinary deprecation keeps its pass. A failing `setUpBeforeClass` or
   `tearDownAfterClass` fails every test of its file. What fails no test (an issue while PHPUnit
   built the suite, a COMMAND that failed with no unit failing) makes the failures not told by
   unit. A test run in a process of its own (`processIsolation`, `#[RunInSeparateProcess]`, a
   `.phpt`) is `not observed`: its child loads the user's bootstrap, and nothing of hushrun's.
6. **Leaving out** happens at `TestRunner\ExecutionStarted`: the tracer finds the suite PHPUnit is
   about to run in that event's caller (`TestRunner::run`'s argument, through
   `debug_backtrace`), after PHPUnit's filters and ordering and before any hook, and removes the
   listed tests from it, and every class suite left empty. A test another one left in depends on
   stays, and a class with dependencies is never split. Nothing is removed when PHPUnit's list
   isn't the plan's, when the wrapper isn't following the process, or when PHPUnit collects
   coverage. PHPUnit counts the tests before the removal, so a run of no test left passes,
   PHPUnit 12's `failOnEmptyTestSuite` included.
   - `--filter` with a negative regular expression was the first try. PCRE's compiled-pattern
     limit is reached near 1,000 test names (measured on PHP 8.3), past which PHPUnit's filter
     matches nothing and the run would silently run no test. Removing from the suite has no limit.

## Costs

The sample: 300 test files of 5 tests each, each file loading one class of its own that opens its
own file once, PHPUnit 12.5.45, PHP 8.3.6, Linux, best of three, wall time, on a shared machine.

| | No adapter | Observed (`unit: test`) |
|---|---|---|
| 1,500 tests | 0.41 s | 0.52 s |

About 0.07 ms a test, most of it in the stream wrapper, which every include goes through. Opcache
is off on the CLI by default; a run with `opcache.enable_cli` wasn't measured.

## Consequences

- A PHPUnit or Pest repository is observed with no file of it changed, wherever COMMAND starts
  PHPUnit's own program in a PHP that reads ini files.
- What a test file read is wide early in a run and narrows as reuse works; a change to a class
  loaded by the first test files runs every test file after them once.
- `composer test` is observed when its script runs PHPUnit or Pest; `php artisan test` and
  paratest are not yet.
- PHPUnit 12's `No tests executed!` and Pest's `pending` count are what a run that left every
  test out prints.

## Known misses

- A value one test file put in a static or a registry while loading (a data provider of another
  test file, which runs while PHPUnit builds the suite), read by a test file that doesn't name
  that file: the provider's file isn't read by it. The audit's second run catches it.
- A `.php` file a test checks with `file_exists` or `is_file` only (a stat, not an open), and a
  directory it lists (`glob`, `scandir`): a new `.php` file is a new source, which runs everything
  (C-31's `shape`), so only a file that changed and was only looked at is missed.
- A class a test reaches only through a string built at run time, in another test file.

## Alternatives considered

- **The configuration's `<extensions>`:** an edit of the repository, and PHPUnit 10 takes no
  extension any other way.
- **Requiring Composer's autoloader from the prepend file** to register the tracer before PHPUnit
  starts: it loads the repository's `files` autoloads earlier than PHPUnit would, and the
  autoloader's path depends on Composer's `bin-dir`.
- **A coverage driver for each test's own list** (`reads: test`): PCOV and Xdebug aren't on most
  machines, and a class constant or an enum case runs no line; each test reads its file's list.
- **Every test file's own includes only** (what it loaded between its start and its end): unsound
  for any class an earlier test loaded, which is most of them.

## Verified

PHPUnit 10.5.67, 11.5.58 and 12.5.45, Pest 3.8.7 (PHPUnit 11.5.56) and 4.7.8 (PHPUnit 12.5.33), PHP
8.3.6 on Linux, through `hushrun run`: test files and tests reused, a data set left out alone, a
test file opening a `.php` file as data, other test files not read, a failing test and a failing
`tearDownAfterClass` never reused, a run of no test left passing on PHPUnit 12, Pest through
`composer test`. Not verified: PHPUnit run as a PHAR, Windows, `opcache.enable_cli`, and the
coverage guard (no coverage driver could be installed here).
