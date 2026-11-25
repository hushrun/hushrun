# hushrun's Bats adapter (ADR 0115). `BASH_ENV` names this file, so every bash process COMMAND
# starts reads it first: bats' own, the scripts its tests run, and what those run. It changes
# nothing a process does; it notes, through a DEBUG trap, each file a command of it ran from, and
# in bats' own processes it says what each test file and test came to, and leaves out what
# hushrun.json lists by bats' own skip. hushrun's `__hushrun-bats` turns the notes into facts.
#
# bats sets a DEBUG trap of its own in its processes, which replaces hushrun's: there hushrun
# notes from inside bats' (`bats_debug_trap`, which gets a first line that calls hushrun's), and
# says how a test, a test file and the suite ended from inside bats' exit traps, each of which
# gets a call just before its `exit`. A function is changed only where its text is the one
# hushrun knows; otherwise nothing of that kind is said, and nothing is reused that relies on it.
#
# Every name here starts with `__hushrun_bats`, and every variable it exports with
# `HUSHRUN_BATS_`. Nothing here may fail a command: `set -e` and `set -u` are the user's.

if [[ -n ${HUSHRUN_BATS_EXE-} && -n ${HUSHRUN_UNITS-} && -z ${__hushrun_bats_loaded-} ]]; then
  __hushrun_bats_loaded=1
  __hushrun_bats_self=${BASH_SOURCE[0]-}
  __hushrun_bats_last=
  __hushrun_bats_seen=$'\n'
  __hushrun_bats_role=
  __hushrun_bats_wanted=
  __hushrun_bats_done=

  # A note: the directory the process was in, a tab, and the file as bash named it.
  __hushrun_bats_note() {
    if [[ -z ${HUSHRUN_BATS_LOG-} || -z ${1-} || $1 == "$__hushrun_bats_self" ]]; then
      return 0
    fi
    case $__hushrun_bats_seen in
    *$'\n'"$1"$'\n'*) return 0 ;;
    esac
    __hushrun_bats_seen+="$1"$'\n'
    printf '%s\t%s\n' "$PWD" "$1" >>"$HUSHRUN_BATS_LOG" 2>/dev/null || :
    return 0
  }

  # `source FILE` and `. FILE`, about to run: the file, when its word expands with no side
  # effect, so that a file none of whose commands runs is still noted.
  __hushrun_bats_sourcing() {
    local word=${BASH_COMMAND#* } file
    case $word in
    *'$('* | *'`'* | *'$['* | *'<('* | *'>('* | *';'* | *'&'* | *'|'* | *'<'* | *'>'* | *$'\n'*) return 0 ;;
    *'${'*[:=?]*'}'* | *BASH_SOURCE* | *FUNCNAME* | *BASH_LINENO* | *'$'[1-9@*#]* | *'${'[1-9@*#]*) return 0 ;;
    esac
    file=$(
      builtin trap - DEBUG
      set +eu
      eval "set -- $word" 2>/dev/null && printf '%s' "${1-}"
    ) 2>/dev/null || return 0
    __hushrun_bats_note "$file"
    return 0
  }

  __hushrun_bats_now() {
    if [[ -n ${EPOCHREALTIME-} ]]; then
      local int=${EPOCHREALTIME%[.,]*} frac=${EPOCHREALTIME#*[.,]}
      __hushrun_bats_ms="$int${frac:0:3}"
    else
      __hushrun_bats_ms=0
    fi
  }

  # Writes $2 to the file $1, whole.
  __hushrun_bats_write() {
    printf '%s' "$2" >"$1.part" 2>/dev/null && mv -f "$1.part" "$1" 2>/dev/null || :
    return 0
  }

  # Changes the function $1, when it is defined and its text holds $2, to hold $3 in its place.
  __hushrun_bats_patch() {
    local def from=$2 to=$3
    def=$(declare -f "$1" 2>/dev/null) || return 1
    case $def in
    *"$from"*) ;;
    *) return 1 ;;
    esac
    # `to` unquoted: bash 3.2 keeps the quotes of a quoted replacement, and none holds a `&`.
    def=${def/"$from"/$to}
    eval "$def" 2>/dev/null || return 1
    return 0
  }

  # Each of the role's functions is changed once, as soon as it is defined.
  __hushrun_bats_patch_role() {
    local name rest= done
    for name in $__hushrun_bats_wanted; do
      done=
      case $name in
      bats_debug_trap)
        # bats may source its tracing twice: its trap's function is changed as bats sets the trap.
        case $BASH_COMMAND in
        trap*bats_debug_trap*DEBUG)
          __hushrun_bats_patch bats_debug_trap '{' '{ __hushrun_bats_in_bats;' && done=1
          ;;
        esac
        ;;
      bats_exit_trap)
        __hushrun_bats_patch bats_exit_trap 'exit "$status"' '__hushrun_bats_test_ended; exit "$status"' && done=1
        ;;
      bats_file_exit_trap)
        __hushrun_bats_patch bats_file_exit_trap 'exit "$bats_exec_file_status"' \
          '__hushrun_bats_file_ended; exit "$bats_exec_file_status"' && done=1
        ;;
      bats_suite_exit_trap)
        __hushrun_bats_patch bats_suite_exit_trap 'exit "$bats_exec_suite_status"' \
          '__hushrun_bats_suite_ended; exit "$bats_exec_suite_status"' && done=1
        ;;
      esac
      [[ -n $done ]] || rest+="$name "
    done
    __hushrun_bats_wanted=$rest
  }

  # What a command about to run says, `$1` frames above the caller.
  __hushrun_bats_at() {
    local depth=$(($1 + 1))
    local file=${BASH_SOURCE[$depth]-}
    if [[ $file != "$__hushrun_bats_last" ]]; then
      __hushrun_bats_last=$file
      __hushrun_bats_note "$file"
    fi
    case $BASH_COMMAND in
    'source '* | '. '* | 'builtin source '* | 'builtin . '* | 'command source '* | 'command . '*)
      __hushrun_bats_sourcing
      ;;
    esac
    if [[ -n $__hushrun_bats_role ]]; then
      [[ -z $__hushrun_bats_wanted ]] || __hushrun_bats_patch_role
      "__hushrun_bats_in_$__hushrun_bats_role" "${FUNCNAME[$depth]-}"
    fi
    return 0
  }

  # hushrun's DEBUG trap, until bats sets its own; `"$_"` keeps `$_` as it was.
  __hushrun_bats_debug() {
    __hushrun_bats_at 1
    return 0
  }

  # From bats' own DEBUG trap, as its first line.
  __hushrun_bats_in_bats() {
    __hushrun_bats_at 2
    return 0
  }

  # The test file a bats process names, from the repository's root, in __hushrun_bats_unit.
  __hushrun_bats_unit_of() {
    local given=$1 dir base abs root
    case $given in
    */*) dir=${given%/*} base=${given##*/} ;;
    *) dir=. base=$given ;;
    esac
    [[ -n $dir ]] || dir=/
    abs=$(cd -P -- "$dir" 2>/dev/null && pwd -P) || abs=
    __hushrun_bats_unit=
    [[ -n $abs ]] || return 0
    abs=${abs%/}/$base
    for root in "${__hushrun_bats_roots[@]-}"; do
      if [[ -n $root && $abs == "${root%/}/"* ]]; then
        __hushrun_bats_unit=${abs#"${root%/}/"}
        return 0
      fi
    done
    return 0
  }

  __hushrun_bats_plan() {
    __hushrun_bats_roots=()
    __hushrun_bats_reused_file() { return 1; }
    __hushrun_bats_reused_test() { return 1; }
    if [[ -n ${HUSHRUN_BATS_RUN-} && -f $HUSHRUN_BATS_RUN/plan.bash ]]; then
      builtin source "$HUSHRUN_BATS_RUN/plan.bash" 2>/dev/null || :
    fi
  }

  # bats' main process: its start, which says so until its facts are written.
  __hushrun_bats_main() {
    local run=$HUSHRUN_UNITS/bats-$$
    mkdir -p "$run" 2>/dev/null || return 0
    export HUSHRUN_BATS_RUN=$run HUSHRUN_BATS_LIBEXEC=${0%/*} HUSHRUN_BATS_LOG=$run/suite.log
    __hushrun_bats_write "$HUSHRUN_UNITS/bats-$$.started" \
      '{"why": "a start of bats ended before hushrun read what it ran"}'
    "$HUSHRUN_BATS_EXE" __hushrun-bats plan "$HUSHRUN_UNITS" "$run" </dev/null >/dev/null 2>&1 || :
    __hushrun_bats_role=main
    __hushrun_bats_rewrap=
    builtin trap -- '__hushrun_bats_finish' EXIT
  }

  __hushrun_bats_action() { __hushrun_bats_was=${3-}; }

  # bats' main process sets an EXIT trap of its own, which replaces hushrun's: at the next command
  # hushrun's goes first again, before bats', which keeps its status. No function of bats' changes.
  __hushrun_bats_in_main() {
    if [[ -n $__hushrun_bats_rewrap ]]; then
      __hushrun_bats_rewrap=
      # Listed into a file, not `$(…)`: a subshell may list its own traps, which are reset.
      local listed= at=$HUSHRUN_BATS_RUN/exit-trap
      if builtin trap -p EXIT >"$at" 2>/dev/null; then
        listed=$(<"$at")
      fi
      __hushrun_bats_was=
      [[ -z $listed ]] || eval "__hushrun_bats_action $listed" 2>/dev/null || :
      case $__hushrun_bats_was in
      '' | __hushrun_bats_finish) builtin trap -- '__hushrun_bats_finish' EXIT ;;
      '__hushrun_bats_finish; '*) ;;
      *) builtin trap -- "__hushrun_bats_finish; $__hushrun_bats_was" EXIT ;;
      esac
    fi
    case $BASH_COMMAND in
    trap*EXIT*) __hushrun_bats_rewrap=1 ;;
    esac
  }

  __hushrun_bats_finish() {
    local status=$?
    if [[ -z $__hushrun_bats_done ]]; then
      __hushrun_bats_done=1
      "$HUSHRUN_BATS_EXE" __hushrun-bats facts "$HUSHRUN_UNITS" "$HUSHRUN_BATS_RUN" </dev/null >/dev/null 2>&1 || :
    fi
    return $status
  }

  # bats-exec-suite: setup_suite and teardown_suite, which every test file reads.
  __hushrun_bats_in_suite() { :; }

  __hushrun_bats_suite_ended() {
    if [[ -z ${BATS_SETUP_SUITE_COMPLETED-} || -z ${BATS_TEARDOWN_SUITE_COMPLETED-} ]]; then
      __hushrun_bats_write "$HUSHRUN_BATS_RUN/suite.failed" "setup_suite or teardown_suite failed"
    fi
    __hushrun_bats_write "$HUSHRUN_BATS_RUN/suite.ended" "1"
    return 0
  }

  # bats-exec-file: one test file, its loading, setup_file and teardown_file.
  __hushrun_bats_file() {
    local given=${*: -2:1} list=${*: -1} line prefix tests=
    __hushrun_bats_id=$HUSHRUN_BATS_RUN/file-$$
    export HUSHRUN_BATS_LOG=$__hushrun_bats_id.log
    __hushrun_bats_unit_of "$given"
    prefix=$given$'\t'
    if [[ -f $list ]]; then
      while IFS= read -r line || [[ -n $line ]]; do
        if [[ $line == "$prefix"* ]]; then
          tests+=${line#"$prefix"}$'\n'
        fi
      done <"$list"
    fi
    __hushrun_bats_file_reused=
    unset HUSHRUN_BATS_FILE_REUSED
    if [[ -n $__hushrun_bats_unit ]] && __hushrun_bats_reused_file "$__hushrun_bats_unit"; then
      # Each of its tests skips as a test hushrun reuses does, in a process of its own: bats'
      # own way to skip a whole file runs the tests' bodies in 1.10, and nothing in 1.11.
      __hushrun_bats_file_reused=1
      export HUSHRUN_BATS_FILE_REUSED=$__hushrun_bats_unit
    fi
    __hushrun_bats_write "$__hushrun_bats_id.tests" "$tests"
    __hushrun_bats_write "$__hushrun_bats_id.meta" "unit $__hushrun_bats_unit"$'\n'"given $given"$'\n'"reused ${__hushrun_bats_file_reused:-0}"$'\n'
  }

  __hushrun_bats_in_file() {
    # A test file hushrun reuses whole runs neither setup_file nor teardown_file.
    if [[ -n $__hushrun_bats_file_reused && $1 == bats_run_setup_file && $BASH_COMMAND == setup_file* ]]; then
      setup_file() { :; }
      teardown_file() { :; }
    fi
  }

  __hushrun_bats_file_ended() {
    if [[ -n ${failure_reason-} ]]; then
      __hushrun_bats_write "$__hushrun_bats_id.failed" "$failure_reason"
    fi
    __hushrun_bats_write "$__hushrun_bats_id.ended" "1"
    return 0
  }

  # bats-exec-test: one try of one test, in a process of its own.
  __hushrun_bats_test() {
    local given=${*: -5:1} name=${*: -4:1} number=${*: -3:1} try=${*: -1}
    __hushrun_bats_id=$HUSHRUN_BATS_RUN/test-$number-$try
    __hushrun_bats_decided=
    export HUSHRUN_BATS_LOG=$__hushrun_bats_id.log HUSHRUN_BATS_INSIDE=1
    __hushrun_bats_unit_of "$given"
    __hushrun_bats_now
    __hushrun_bats_write "$__hushrun_bats_id.meta" \
      "unit $__hushrun_bats_unit"$'\n'"given $given"$'\n'"name $name"$'\n'"number $number"$'\n'"try $try"$'\n'"start $__hushrun_bats_ms"$'\n'
  }

  __hushrun_bats_in_test() {
    # Just before bats calls `setup`, the test has its description, in every bats: 1.10 sets it
    # in bats_test_begin, from the test's own function, and 1.11 and later before.
    if [[ -z $__hushrun_bats_decided && $BASH_COMMAND == setup* ]]; then
      case $1 in
      bats_perform_test | bats_test_begin) ;;
      *) return 0 ;;
      esac
      __hushrun_bats_decided=1
      local id=${BATS_TEST_DESCRIPTION-} reused=0
      if [[ -n $__hushrun_bats_unit ]] && { [[ ${HUSHRUN_BATS_FILE_REUSED-} == "$__hushrun_bats_unit" ]] ||
        __hushrun_bats_reused_test "$__hushrun_bats_unit" "$id"; }; then
        reused=1
        setup() { skip 'reused by hushrun'; }
        teardown() { :; }
      fi
      __hushrun_bats_write "$__hushrun_bats_id.id" "$id"
      __hushrun_bats_write "$__hushrun_bats_id.reused" "$reused"
    fi
  }

  # Inside bats_exit_trap, before its `exit`: `state` is `passed` or `failed`, and unset for a
  # try bats runs again.
  __hushrun_bats_test_ended() {
    if [[ -n ${state-} ]]; then
      local skipped=0
      [[ -z ${BATS_TEST_SKIPPED-} ]] || skipped=1
      __hushrun_bats_now
      __hushrun_bats_write "$__hushrun_bats_id.end" \
        "state $state"$'\n'"skipped $skipped"$'\n'"end $__hushrun_bats_ms"$'\n'
    fi
    return 0
  }

  # Which process this is, from its script's name and arguments: at its first command, not while
  # bash reads `BASH_ENV`, before which bash 3.2 has yet to set `$0` and the arguments.
  __hushrun_bats_start() {
    if [[ -z ${HUSHRUN_BATS_INSIDE-} ]]; then
      if [[ -z ${HUSHRUN_BATS_RUN-} ]]; then
        if [[ ${0##*/} == bats && -f ${0%/*}/bats-exec-suite && -f ${0%/*}/bats-exec-test ]]; then
          __hushrun_bats_main
        fi
      elif [[ -n ${HUSHRUN_BATS_LIBEXEC-} && ${0%/*} -ef $HUSHRUN_BATS_LIBEXEC ]]; then
        __hushrun_bats_plan
        case ${0##*/} in
        bats-exec-suite)
          __hushrun_bats_role=suite
          __hushrun_bats_wanted='bats_debug_trap bats_suite_exit_trap'
          __hushrun_bats_write "$HUSHRUN_BATS_RUN/version" "${BATS_VERSION-}"
          ;;
        bats-exec-file)
          __hushrun_bats_role=file
          __hushrun_bats_wanted='bats_debug_trap bats_file_exit_trap'
          __hushrun_bats_file "$@"
          ;;
        bats-exec-test)
          __hushrun_bats_role=test
          __hushrun_bats_wanted='bats_debug_trap bats_exit_trap'
          __hushrun_bats_test "$@"
          ;;
        bats)
          # bats started again inside this run (from setup_file or setup_suite): what it runs is
          # read by what started it.
          export HUSHRUN_BATS_INSIDE=1
          ;;
        esac
      elif [[ ${0##*/} == bats && -f ${0%/*}/bats-exec-suite ]]; then
        export HUSHRUN_BATS_INSIDE=1
      fi
    fi

    # A script bash runs is read whole before its first command.
    if [[ -z ${BASH_EXECUTION_STRING-} && -f $0 ]]; then
      __hushrun_bats_note "$0"
    fi
  }

  # hushrun's first DEBUG trap: the script's own arguments, then `$_`. At the first command that
  # isn't this file's, nor of a file it is sourcing, it says which process this is, and from then
  # on hushrun's DEBUG trap is the one that takes `$_` alone.
  __hushrun_bats_first() {
    local i n=$#
    for ((i = 1; i < ${#BASH_SOURCE[@]}; i++)); do
      [[ ${BASH_SOURCE[i]} != "$__hushrun_bats_self" ]] || return 0
    done
    builtin trap -- '__hushrun_bats_debug "$_"' DEBUG
    if ((n > 1)); then
      __hushrun_bats_start "${@:1:n-1}"
    else
      __hushrun_bats_start
    fi
    __hushrun_bats_at 1
    return 0
  }

  # Where bash has set `$0` already (4 and later), a script that can't parse still read itself.
  if [[ -z ${BASH_EXECUTION_STRING-} && -f $0 ]]; then
    __hushrun_bats_note "$0"
  fi
  set -T
  # `${1+"$@"}`: bash 3.2's `"$@"` with no arguments fails under `set -u`.
  builtin trap -- '__hushrun_bats_first ${1+"$@"} "$_"' DEBUG
fi

# The BASH_ENV COMMAND's environment had, read as bash would have.
if [[ -n ${HUSHRUN_BATS_ENV_WAS-} && -r $HUSHRUN_BATS_ENV_WAS ]]; then
  builtin source "$HUSHRUN_BATS_ENV_WAS"
fi
