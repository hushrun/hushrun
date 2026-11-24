%% hushrun's ExUnit adapter (ADR 0122, spec C-114 to C-117). hushrun compiles this file once into
%% its cache directory and loads it into every Erlang VM of COMMAND through ERL_LIBS and
%% ELIXIR_ERL_OPTIONS="-s hushrun_exunit start". In a VM without HUSHRUN_UNITS, and after any error
%% of its own, it does nothing and Mix runs as without it.
%%
%% What it observes, for each test file `mix test` runs:
%%
%% - What the file's code references: Elixir's own lexical tracker for the file, read by a compiler
%%   tracer when the file's compilation stops, as `mix test --stale` reads it (compile, export and
%%   runtime references).
%% - What those modules are made of, to the end: every Mix compile manifest of an application whose
%%   folder is inside the repository (the project, an umbrella's apps, path dependencies), each
%%   source with the modules it references and its external resources; a module no manifest names
%%   (Erlang code) by its beam's source and the modules its beam imports.
%% - What ran: a call counter on each exported function of the repository's modules. The counters
%%   are read each time a test module starts or finishes; what was called in between was called
%%   by every test file running then (async test files run at once), and what was called while no
%%   test file ran (the application starting, a test helper) by every test file that ends after.
%%   Each module that ran is read with what it references, to the end.
%% - Files Elixir compiled or evaluated while test files ran (`Code.require_file`,
%%   `Code.eval_file`), as the files that ran.
%% - The repository's configuration: `mix.exs`, `mix.lock`, the files of the config folder and the
%%   test helpers, read by every test file.
%%
%% What it can't follow, it reports: a port (`System.cmd`, `:os.cmd`) opened while a test file ran
%% (`subprocess`), and a compile manifest it can't read (`manifest`).
%%
%% It deselects (ADR 0069) the test files hushrun.json's `reuse` lists, through ExUnit's own
%% filters: once the test helper has run, it adds `{file, Path}` to ExUnit's `exclude`, which
%% `mix test` keeps beside its own. Only the first VM to start with the run's directory may: it
%% claims it. It decides nothing: hushrun decides each test file's state from these facts.
%%
%% Output: one file per test suite in $HUSHRUN_UNITS, written whole (a temporary file, then a
%% rename) when the suite finishes.

-module(hushrun_exunit).
-behaviour(gen_server).

-export([start/0, trace/2]).
-export([init/1, handle_call/3, handle_cast/2, handle_info/2, terminate/2, code_change/3]).
-export([decode/1, encode/1, closure/2]).

-define(TABLE, hushrun_exunit).
-define(SCHEMA, <<"hushrun.unit-facts/v0">>).
-define(WORD, <<"exunit">>).
-define(CLAIM, "exunit-claim").

%% --- start ---------------------------------------------------------------------------------

start() ->
    case os:getenv("HUSHRUN_UNITS") of
        false -> ok;
        "" -> ok;
        Dir ->
            try boot(Dir)
            catch Class:Reason:Stack ->
                write_errors(Dir, [describe(Class, Reason, Stack)])
            end
    end,
    ok.

boot(Dir) ->
    {ok, Bytes} = file:read_file(filename:join(Dir, "hushrun.json")),
    Run = decode(Bytes),
    Root = maps:get(<<"root">>, Run),
    Reuse = [R || R <- maps:get(<<"reuse">>, Run, []), is_binary(R)],
    Claimed = case file:open(filename:join(Dir, ?CLAIM), [write, exclusive]) of
                  {ok, F} -> ok = file:write(F, os:getpid()), ok = file:close(F), true;
                  _ -> false
              end,
    Self = self(),
    Owner = spawn(fun() -> own(Self) end),
    receive {Owner, ready} -> ok after 5000 -> erlang:error(no_table) end,
    ets:insert(?TABLE, [{dir, Dir}, {root, Root}, {reuse, Reuse}, {claimed, Claimed},
                        {start, hex(crypto:strong_rand_bytes(16))}, {suites, 0}, {errors, []}]),
    {ok, _} = application:ensure_all_started(elixir),
    Tracers = 'Elixir.Code':get_compiler_option(tracers),
    'Elixir.Code':put_compiler_option(tracers, [?MODULE | Tracers -- [?MODULE]]),
    ok = case application:load(ex_unit) of
             ok -> ok;
             {error, {already_loaded, ex_unit}} -> ok
         end,
    Formatters = case application:get_env(ex_unit, formatters) of
                     {ok, L} when is_list(L) -> L;
                     _ -> ['Elixir.ExUnit.CLIFormatter']
                 end,
    application:set_env(ex_unit, formatters, Formatters ++ [?MODULE]),
    ok.

%% The table's owner: a process of its own, so that the table lives as long as the VM.
own(Parent) ->
    ?TABLE = ets:new(?TABLE, [named_table, public, set, {read_concurrency, true},
                              {write_concurrency, true}]),
    Parent ! {self(), ready},
    receive never -> ok end.

%% --- the compiler tracer -------------------------------------------------------------------

trace(stop, Env) ->
    guard(fun() -> stopped(Env) end);
trace({on_module, _, _}, Env) ->
    guard(fun() -> defined(Env) end);
trace(_, _) ->
    ok.

guard(Fun) ->
    try Fun()
    catch Class:Reason:Stack -> note_error(describe(Class, Reason, Stack))
    end,
    ok.

%% A file's compilation stopped: what its code references, as `mix test --stale` reads it.
stopped(#{file := File, lexical_tracker := Tracker}) when is_pid(Tracker) ->
    {Compile, Export, Runtime, _} = 'Elixir.Kernel.LexicalTracker':references(Tracker),
    Refs = lists:usort(Compile ++ Export ++ Runtime),
    Old = case ets:lookup(?TABLE, {refs, File}) of
              [{_, Prev}] -> Prev;
              [] -> []
          end,
    ets:insert(?TABLE, {{refs, File}, lists:umerge(Old, Refs)}),
    ets:insert(?TABLE, {{evaluated, erlang:unique_integer([monotonic, positive])}, File}),
    case filename:basename(File) of
        <<"test_helper.exs">> -> helper_ran(File);
        _ -> ok
    end;
stopped(_) ->
    ok.

%% A module was defined from a file compiled in this VM (a test module, a test helper's).
defined(#{file := File, module := Module}) when is_atom(Module), Module =/= nil ->
    ets:insert(?TABLE, {{defined, Module}, File}),
    case ets:lookup(?TABLE, counting) of
        [{counting, true}] -> count(Module);
        _ -> ok
    end;
defined(_) ->
    ok.

%% [ADR 0069] The test helper ran, and `mix test` is about to start ExUnit's run: the test files
%% `reuse` lists are left out by ExUnit's own filter on their `file` tag.
helper_ran(Helper) ->
    ets:insert(?TABLE, {{helper, Helper}, true}),
    case lookup(claimed) of
        true ->
            Root = lookup(root),
            Paths = [filename:join(Root, R) || R <- lookup(reuse)],
            Exclude = case application:get_env(ex_unit, exclude) of
                          {ok, E} when is_list(E) -> E;
                          _ -> []
                      end,
            Added = [{file, P} || P <- Paths] -- Exclude,
            application:set_env(ex_unit, exclude, Exclude ++ Added),
            ets:insert(?TABLE, {left_out, Paths});
        _ ->
            ok
    end.

%% --- the formatter -------------------------------------------------------------------------

init(_Opts) ->
    try
        Graph = graph(),
        Modules = lists:usort(maps:keys(maps:get(modules, Graph)) ++ maps:get(beams, Graph)),
        %% Every module of the repository is loaded now, as `mix test --slowest` has Mix do, so
        %% that a counter is on it before its first call.
        [catch code:ensure_loaded(M) || M <- Modules],
        ets:insert(?TABLE, [{counting, true}, {project, Modules}, {kind, counters()}]),
        [catch erlang:trace_pattern(MFA, true, [lookup(kind)]) || MFA <- [{erlang, open_port, 2}]],
        _ = loaded(),
        _ = entered(),
        %% What was compiled before the suite started is the project's own build.
        _ = evaluated(),
        _ = ports(),
        N = ets:update_counter(?TABLE, suites, 1),
        Name = lists:flatten(io_lib:format("exunit-~s-~b", [os:getpid(), N])),
        Started = filename:join(lookup(dir), Name ++ ".started"),
        ok = file:write_file(Started, <<>>),
        {ok, #{name => Name, started => Started, graph => Graph, seq => 0, active => #{},
               files => #{}, batches => [], pids => #{}, direct => #{}, spawned => #{},
               failed => false}}
    catch Class:Reason:Stack ->
        note_error(describe(Class, Reason, Stack)),
        {ok, #{failed => true}}
    end.

handle_cast(_, #{failed := true} = S) ->
    {noreply, S};
handle_cast(Event, S) ->
    try {noreply, event(Event, S)}
    catch Class:Reason:Stack ->
        note_error(describe(Class, Reason, Stack)),
        {noreply, S#{failed => true}}
    end.

handle_call(_, _, S) -> {reply, ok, S}.
handle_info(_, S) -> {noreply, S}.
terminate(_, _) -> ok.
code_change(_, S, _) -> {ok, S}.

event({module_started, #{file := File}}, S) ->
    S1 = snapshot(S, [File]),
    #{seq := Seq, active := Active, files := Files} = S1,
    Unit = unit(File, Files),
    S1#{active => Active#{File => true},
        files => Files#{File => Unit#{start => Seq}}};
event({module_finished, #{file := File} = Module}, S) ->
    S1 = snapshot(S, []),
    #{seq := Seq, active := Active, files := Files} = S1,
    Unit = unit(File, Files),
    Failed = case maps:get(state, Module, nil) of
                 nil -> maps:get(failed, Unit);
                 _ -> true
             end,
    S1#{active => maps:remove(File, Active),
        files => Files#{File => Unit#{stop => Seq, failed => Failed}}};
event({test_finished, #{tags := #{file := File}} = Test}, #{files := Files} = S) ->
    Unit = unit(File, Files),
    #{selected := Selected, finished := Finished, skipped := Skipped,
      excluded := Excluded, duration := Duration} = Unit,
    Time = case maps:get(time, Test, 0) of T when is_integer(T) -> T; _ -> 0 end,
    Unit1 = case maps:get(state, Test, nil) of
                nil -> Unit#{selected => Selected + 1, finished => Finished + 1};
                {failed, _} -> Unit#{selected => Selected + 1, finished => Finished + 1,
                                     failed => true};
                {invalid, _} -> Unit#{selected => Selected + 1, failed => true};
                {skipped, _} -> Unit#{selected => Selected + 1, finished => Finished + 1,
                                      skipped => Skipped + 1};
                {excluded, _} -> Unit#{excluded => Excluded + 1};
                _ -> Unit#{selected => Selected + 1}
            end,
    S#{files => Files#{File => Unit1#{duration => Duration + Time}}};
event({suite_finished, _}, S) ->
    S1 = snapshot(S, []),
    write(S1),
    S1;
event(_, S) ->
    S.

unit(File, Files) ->
    maps:get(File, Files, #{selected => 0, finished => 0, failed => false, skipped => 0,
                            excluded => 0, duration => 0, start => undefined,
                            stop => undefined}).

%% What was called since the last snapshot, by which process. A process that called a test
%% module's function, or was started by one that did (`$callers`, `$ancestors`), is its test
%% file's: what it called is that file's. What any other process called (an application's own
%% server), and the files evaluated, were called for every test file running, every one whose
%% start is still in this process's mailbox and `Starting`; with none, for every test file that
%% ends after.
snapshot(#{seq := Seq, active := Active, batches := Batches, pids := Pids0,
           direct := Direct0, spawned := Spawned0} = S, Starting) ->
    New = loaded(),
    Calls = entered(),
    Opened = ports(),
    Evaluated = evaluated(),
    Pids1 = maps:fold(fun learn/3, Pids0, Calls),
    {Direct, Loose, Pids2} =
        maps:fold(fun(Pid, Mods, {D, L, P}) ->
                          case owner(Pid, P) of
                              {none, P1} -> {D, L#{Pid => Mods}, P1};
                              {File, P1} ->
                                  {maps:update_with(File, fun(Old) -> Mods ++ Old end, Mods, D),
                                   L, P1}
                          end
                  end, {Direct0, #{}, Pids1}, Calls),
    {Spawned, LoosePorts, Pids3} =
        lists:foldl(fun(Pid, {Sp, L, P}) ->
                            case owner(Pid, P) of
                                {none, P1} -> {Sp, L + 1, P1};
                                {File, P1} -> {Sp#{File => true}, L, P1}
                            end
                    end, {Spawned0, 0, Pids2}, Opened),
    Readers = lists:usort(maps:keys(Active) ++ pending_starts() ++ Starting),
    Batch = #{seq => Seq, modules => New, calls => Loose, files => Evaluated,
              readers => Readers, gap => Readers =:= [], ports => LoosePorts},
    Batches1 = case {New, maps:size(Loose), Evaluated, LoosePorts} of
                   {[], 0, [], 0} -> Batches;
                   _ -> [Batch | Batches]
               end,
    S#{seq => Seq + 1, batches => Batches1, pids => Pids3,
       direct => maps:map(fun(_, Mods) -> lists:usort(Mods) end, Direct),
       spawned => Spawned}.

%% A process that called the functions of one test file's modules is that file's.
learn(unknown, _, Pids) ->
    Pids;
learn(Pid, Mods, Pids) ->
    Files = lists:usort([F || M <- Mods, [{_, F}] <- [ets:lookup(?TABLE, {defined, M})],
                              is_test_file(F)]),
    case Files of
        [File] -> Pids#{Pid => File};
        _ -> Pids
    end.

is_test_file(File) ->
    binary:longest_common_suffix([File, <<"_test.exs">>]) =:= 9.

%% The test file a process is, or was started for, as far as its own dictionary tells.
owner(unknown, Pids) ->
    {none, Pids};
owner(Pid, Pids) ->
    case maps:find(Pid, Pids) of
        {ok, File} -> {File, Pids};
        error ->
            Dictionary = case catch erlang:process_info(Pid, dictionary) of
                             {dictionary, D} -> D;
                             _ -> []
                         end,
            Up = [whereis_pid(X) || Key <- ['$callers', '$ancestors'],
                                    {K, L} <- Dictionary, K =:= Key, is_list(L), X <- L],
            case [F || U <- Up, {ok, F} <- [maps:find(U, Pids)]] of
                [File | _] -> {File, Pids#{Pid => File}};
                [] ->
                    case started_for(Pid, Pids, 64) of
                        none -> {none, Pids};
                        File -> {File, Pids#{Pid => File}}
                    end
            end
    end.

%% The test file a process was started for: the test module its start names (ExUnit starts a
%% test module's process with a function that holds the module), or its parent's, up the line.
started_for(_, _, 0) ->
    none;
started_for(Pid, Pids, N) ->
    case ets:lookup(?TABLE, {parent, Pid}) of
        [{_, Parent, Start}] ->
            case test_module_of(Start) of
                none ->
                    case maps:find(Parent, Pids) of
                        {ok, File} -> File;
                        error -> started_for(Parent, Pids, N - 1)
                    end;
                File -> File
            end;
        [] -> none
    end.

test_module_of({erlang, apply, [Fun | _]}) when is_function(Fun) ->
    {env, Env} = erlang:fun_info(Fun, env),
    %% The module alone, or with its parameters (`{Module, Params}`, from Elixir 1.18).
    Atoms = [A || X <- Env, A <- case X of
                                     _ when is_atom(X) -> [X];
                                     {M, _} when is_atom(M) -> [M];
                                     _ -> []
                                 end],
    Files = lists:usort([F || A <- Atoms, [{_, F}] <- [ets:lookup(?TABLE, {defined, A})],
                              is_test_file(F)]),
    case Files of
        [File] -> File;
        _ -> none
    end;
test_module_of(_) ->
    none.

whereis_pid(P) when is_pid(P) -> P;
whereis_pid(A) when is_atom(A) -> whereis(A);
whereis_pid(_) -> undefined.

pending_starts() ->
    {messages, Messages} = erlang:process_info(self(), messages),
    [File || {'$gen_cast', {module_started, #{file := File}}} <- Messages].

%% [C-116] Counters by process need the processes traced for calls: all of them, by a tracer
%% that is sent nothing (no function has a trace pattern that sends a message). Where another
%% tracer holds a process already, counters are of every process at once.
counters() ->
    Tracer = spawn(fun spawns/0),
    case catch erlang:trace(all, true, [call, procs, {tracer, Tracer}]) of
        N when is_integer(N) -> call_time;
        _ -> exit(Tracer, kill), call_count
    end.

%% The tracer: it keeps who started each process, and with what, and drops everything else.
spawns() ->
    receive
        {trace, Parent, spawn, Child, Start} ->
            ets:insert(?TABLE, {{parent, Child}, Parent, Start});
        _ ->
            ok
    end,
    spawns().

%% [C-116] A call counter, by process, on every exported function of a module of the repository.
count(Module) ->
    case erlang:module_loaded(Module) of
        true ->
            catch erlang:trace_pattern({Module, '_', '_'}, true, [lookup(kind)]),
            ets:insert(?TABLE, {{counted, Module}, true});
        false -> ok
    end,
    ok.

%% The modules of the repository loaded since the last time, which get their counters now. A call
%% loaded each (Erlang loads a module the first time one of its functions is called, before any
%% counter can be on it), by a process that can't be told.
loaded() ->
    Modules = lookup(project) ++ defined_modules(),
    New = [M || M <- lists:usort(Modules), erlang:module_loaded(M),
                ets:lookup(?TABLE, {counted, M}) =:= []],
    lists:foreach(fun count/1, New),
    New.

%% For each process, the modules of the repository a function of which it called since the last
%% time, their counters set back to zero.
entered() ->
    Modules = lookup(project) ++ defined_modules(),
    lists:foldl(fun calls/2, #{}, lists:usort([M || M <- Modules, erlang:module_loaded(M)])).

calls(Module, Acc) ->
    %% Never Module:module_info/1, which is a call of the module's own.
    Exports = try erlang:get_module_info(Module, exports) catch _:_ -> [] end,
    {Acc1, Hit} =
        lists:foldl(fun({F, _}, Done) when F =:= module_info; F =:= '__info__';
                                           F =:= behaviour_info -> Done;
                       ({F, A}, {In, H}) ->
                            case erlang:trace_info({Module, F, A}, lookup(kind)) of
                                {call_time, List} when is_list(List) ->
                                    Pids = [P || {P, C, _, _} <- List, C > 0],
                                    {lists:foldl(fun(P, X) ->
                                                         maps:update_with(P, fun(L) -> [Module | L] end,
                                                                          [Module], X)
                                                 end, In, Pids),
                                     H orelse Pids =/= []};
                                {call_count, C} when is_integer(C), C > 0 ->
                                    Pids = [unknown],
                                    {lists:foldl(fun(P, X) ->
                                                         maps:update_with(P, fun(L) -> [Module | L] end,
                                                                          [Module], X)
                                                 end, In, Pids),
                                     H orelse Pids =/= []};
                                _ -> {In, H}
                            end
                    end, {Acc, false}, Exports),
    case Hit of
        true -> catch erlang:trace_pattern({Module, '_', '_'}, restart, [lookup(kind)]);
        false -> ok
    end,
    Acc1.

%% The processes that called `MFA` since the last time.
pids_of(MFA) ->
    Pids = case catch erlang:trace_info(MFA, lookup(kind)) of
               {call_time, List} when is_list(List) -> [P || {P, C, _, _} <- List, C > 0];
               {call_count, C} when is_integer(C), C > 0 -> [unknown];
               _ -> []
           end,
    catch erlang:trace_pattern(MFA, restart, [lookup(kind)]),
    Pids.

%% The processes that opened a port since the last time.
ports() ->
    pids_of({erlang, open_port, 2}).

evaluated() ->
    Found = ets:match_object(?TABLE, {{evaluated, '_'}, '_'}),
    [ets:delete(?TABLE, Key) || {Key, _} <- Found],
    lists:usort([File || {_, File} <- Found]).

defined_modules() ->
    [M || [M] <- ets:match(?TABLE, {{defined, '$1'}, '_'})].

%% --- what the repository's code is made of -------------------------------------------------

%% Every compile manifest of an application inside the repository: each module's sources, and
%% each source's references and external resources, by absolute path.
graph() ->
    Root = lookup(root),
    Build = 'Elixir.Mix.Project':build_path(),
    Deps = 'Elixir.Mix.Project':deps_paths(),
    {ok, Cwd} = file:get_cwd(),
    App = proplists:get_value(app, 'Elixir.Mix.Project':config()),
    Manifests = filelib:wildcard(binary_to_list(iolist_to_binary(
                                   [Build, "/lib/*/.mix/compile.elixir"]))),
    lists:foldl(
      fun(Manifest, G) ->
              Name = list_to_atom(filename:basename(filename:dirname(filename:dirname(Manifest)))),
              Base = case Name of
                         App -> list_to_binary(Cwd);
                         _ -> iolist_to_binary(filename:absname(maps:get(Name, Deps, <<"/">>)))
                     end,
              case inside(Base, Root) andalso not inside(Base, filename:join(Root, "deps")) of
                  true ->
                      Ebin = filename:join(filename:dirname(filename:dirname(Manifest)), "ebin"),
                      Beams = [list_to_atom(filename:basename(B, ".beam"))
                               || B <- filelib:wildcard(filename:join(Ebin, "*.beam"))],
                      G1 = manifest(Manifest, Base, G),
                      G1#{beams => Beams ++ maps:get(beams, G1)};
                  false -> G
              end
      end, #{modules => #{}, sources => #{}, unread => [], beams => []}, Manifests).

manifest(Path, Base, #{modules := Ms, sources := Ss, unread := Unread} = G) ->
    case 'Elixir.Mix.Compilers.Elixir':read_manifest(list_to_binary(Path)) of
        {[], []} ->
            G#{unread => [list_to_binary(Path) | Unread]};
        {Modules, Sources} ->
            Ms1 = lists:foldl(
                    fun({Name, Record}, Acc) ->
                            Files = [abs(Base, F) || F <- strings(Record)],
                            maps:update_with(Name, fun(Old) -> lists:usort(Old ++ Files) end,
                                             lists:usort(Files), Acc)
                    end, Ms, entries(Modules)),
            Ss1 = lists:foldl(
                    fun({File, Record}, Acc) ->
                            Acc#{abs(Base, File) => #{refs => atoms(Record),
                                                      external => [abs(Base, X) ||
                                                                      X <- strings(Record),
                                                                      X =/= File]}}
                    end, Ss, entries(Sources)),
            G#{modules => Ms1, sources => Ss1};
        _ ->
            G#{unread => [list_to_binary(Path) | Unread]}
    end.

%% A manifest's modules or sources, as `{Key, Record}`: a map from Elixir 1.15, a list of records
%% before, whose key is their second element.
entries(Map) when is_map(Map) ->
    maps:to_list(Map);
entries(List) when is_list(List) ->
    [{element(2, R), R} || R <- List, is_tuple(R), tuple_size(R) >= 2].

%% Every atom of the record's lists: the references of a source, of any kind.
atoms(Record) ->
    lists:usort([A || L <- tuple_to_list(Record), is_list(L), A <- L, is_atom(A),
                      A =/= true, A =/= false, A =/= nil]).

%% Every string of the record's lists, alone or first in a tuple: a module's sources, a source's
%% external resources.
strings(Record) ->
    lists:usort([S || L <- tuple_to_list(Record), is_list(L), X <- L,
                      S <- [string_of(X)], S =/= none]).

string_of(B) when is_binary(B) -> B;
string_of(T) when is_tuple(T), tuple_size(T) >= 1, is_binary(element(1, T)) -> element(1, T);
string_of(_) -> none.

%% The files `Modules` and `Files` read, to the end, with the edges between them.
closure(Graph, Seeds) ->
    walk(Graph, Seeds, #{}, #{}, []).

walk(_, [], Files, _, Edges) ->
    {maps:keys(Files), Edges};
walk(G, [{module, M} | Rest], Files, Seen, Edges) ->
    case maps:is_key({module, M}, Seen) of
        true -> walk(G, Rest, Files, Seen, Edges);
        false ->
            Sources = sources_of(G, M),
            walk(G, [{file, F} || F <- Sources] ++ Rest, Files, Seen#{{module, M} => true},
                 Edges)
    end;
walk(G, [{file, F} | Rest], Files, Seen, Edges) ->
    case maps:is_key(F, Files) of
        true -> walk(G, Rest, Files, Seen, Edges);
        false ->
            {Refs, External} = refs_of(G, F),
            Targets = lists:usort(lists:append([sources_of(G, M) || M <- Refs])),
            New = [[F, T, <<"import">>] || T <- Targets, T =/= F] ++
                  [[F, X, <<"open">>] || X <- External],
            walk(G, [{module, M} || M <- Refs] ++ [{file, X} || X <- External] ++ Rest,
                 Files#{F => true}, Seen, New ++ Edges)
    end.

sources_of(#{modules := Ms}, M) ->
    case maps:find(M, Ms) of
        {ok, Files} -> Files;
        error ->
            case ets:lookup(?TABLE, {defined, M}) of
                [{_, File}] -> [File];
                [] -> beam_source(M)
            end
    end.

refs_of(#{sources := Ss}, F) ->
    case maps:find(F, Ss) of
        {ok, #{refs := Refs, external := External}} -> {Refs, External};
        error ->
            case ets:lookup(?TABLE, {refs, F}) of
                [{_, Refs}] -> {Refs, []};
                [] -> {beam_imports(F), []}
            end
    end.

%% A module no manifest names, such as one of Erlang: its beam's source, when it is the
%% repository's.
beam_source(M) ->
    Root = lookup(root),
    case code:which(M) of
        Beam when is_list(Beam) ->
            case beam_lib:chunks(Beam, [compile_info]) of
                {ok, {_, [{compile_info, Info}]}} ->
                    case proplists:get_value(source, Info) of
                        Source when is_list(Source) ->
                            S = list_to_binary(Source),
                            case inside(S, Root) of
                                true ->
                                    ets:insert(?TABLE, {{beam, S}, Beam}),
                                    [S];
                                false -> []
                            end;
                        _ -> []
                    end;
                _ -> []
            end;
        _ -> []
    end.

beam_imports(Source) ->
    case ets:lookup(?TABLE, {beam, Source}) of
        [{_, Beam}] ->
            case beam_lib:chunks(Beam, [imports]) of
                {ok, {_, [{imports, Imports}]}} -> lists:usort([M || {M, _, _} <- Imports]);
                _ -> []
            end;
        [] -> []
    end.

%% --- the facts file ------------------------------------------------------------------------

write(#{name := Name, started := Started, graph := Graph, files := Files,
        batches := Batches0, direct := Direct0, spawned := Spawned, pids := Pids}) ->
    %% A process told apart only after its calls were counted: they were its test file's.
    {Batches, Direct} = lists:mapfoldl(
                          fun(#{calls := Calls} = B, D) ->
                                  {Loose, D1} = maps:fold(
                                                  fun(Pid, Mods, {L, Dx}) ->
                                                          case owner(Pid, Pids) of
                                                              {none, _} -> {Mods ++ L, Dx};
                                                              {F, _} ->
                                                                  {L, maps:update_with(F, fun(O) -> Mods ++ O end, Mods, Dx)}
                                                          end
                                                  end, {[], D}, Calls),
                                  {B#{modules => lists:usort(maps:get(modules, B) ++ Loose)}, D1}
                          end, Direct0, Batches0),
    Root = lookup(root),
    Dir = lookup(dir),
    Every = every_test(Graph),
    LeftOut = case ets:lookup(?TABLE, left_out) of
                  [{_, L}] -> L;
                  [] -> []
              end,
    Unread = [#{<<"why">> => <<"manifest">>,
                <<"detail">> => iolist_to_binary(["Mix's compile manifest ", rel(Root, U),
                                                  " is of a version hushrun can't read"])}
              || U <- maps:get(unread, Graph)],
    {Units, AllEdges} = lists:foldl(
              fun({File, Unit}, {Acc, EdgesAcc}) ->
                      case rel(Root, File) of
                          none -> {Acc, EdgesAcc};
                          Rel ->
                              {Facts, Edges} = unit_facts(File, Rel, Unit, Files, Graph,
                                                          Batches, Every, LeftOut, Root,
                                                          maps:get(File, Direct, []),
                                                          maps:is_key(File, Spawned)),
                              {[Facts | Acc], Edges ++ EdgesAcc}
                      end
              end, {[], []}, maps:to_list(Files)),
    Edges = lists:usort([[A, B, K] || [F, T, K] <- AllEdges,
                                      A <- [rel(Root, F)], B <- [rel(Root, T)],
                                      A =/= none, B =/= none, A =/= B]),
    Removed = lists:sum([maps:get(excluded, U) || {F, U} <- maps:to_list(Files),
                                                  lists:member(F, LeftOut)]),
    Doc = #{<<"schema">> => ?SCHEMA,
            <<"adapter">> => ?WORD,
            <<"failuresByUnit">> => true,
            <<"start">> => lookup(start),
            <<"settings">> => #{<<"elixir">> => 'Elixir.System':version(),
                                <<"otp">> => list_to_binary(erlang:system_info(otp_release)),
                                <<"claimed">> => lookup(claimed)},
            <<"notFollowed">> => Unread,
            <<"units">> => Units,
            <<"edges">> => Edges,
            <<"removedItems">> => Removed,
            <<"errors">> => lookup(errors)},
    ok = write_whole(Dir, Name ++ ".json", encode(Doc)),
    ok = file:delete(Started).

unit_facts(File, Rel, Unit, Units, Graph, Batches, Every, LeftOut, Root, Called, Opened) ->
    #{selected := Selected, finished := Finished, failed := Failed, skipped := Skipped,
      duration := Duration, start := Start, stop := Stop} = Unit,
    Read = [B || B <- Batches, reads(File, Start, Stop, B)],
    Seeds = [{file, File} | [{file, F} || F <- Every]] ++
            [{module, M} || M <- Called, not of_unit(M, Units)] ++
            [{module, M} || B <- Read, M <- maps:get(modules, B), not of_unit(M, Units)] ++
            [{file, F} || B <- Read, F <- maps:get(files, B), not maps:is_key(F, Units),
                          filename:basename(F) =/= <<"mix.exs">>],
    {Reads, Edges} = closure(Graph, Seeds),
    Spawned = lists:sum([maps:get(ports, B) || B <- Read, not maps:get(gap, B)]),
    NotFollowed = case Spawned =:= 0 andalso not Opened of
                      true -> [];
                      _ -> [#{<<"why">> => <<"subprocess">>,
                              <<"detail">> => <<"a port was opened (System.cmd, :os.cmd, Port.open) "
                                                "while it ran">>}]
                  end,
    Rels = lists:usort([R || F <- Reads, R <- [rel(Root, F)], R =/= none]),
    Facts = #{<<"unit">> => Rel,
              <<"selected">> => Selected,
              <<"finished">> => Finished,
              <<"failed">> => Failed,
              <<"skipped">> => Skipped,
              <<"deselected">> => Selected =:= 0 andalso lists:member(File, LeftOut),
              <<"durationMs">> => Duration div 1000,
              <<"reads">> => Rels,
              <<"notFollowed">> => NotFollowed},
    {Facts, [E || [F, _, _] = E <- Edges, F =:= File]}.

%% A module a test file defines, which the runner calls and is that file's alone, or one a
%% `mix.exs` defines, which Mix calls.
of_unit(Module, Units) ->
    case ets:lookup(?TABLE, {defined, Module}) of
        [{_, File}] -> maps:is_key(File, Units) orelse filename:basename(File) =:= <<"mix.exs">>;
        [] -> false
    end.

%% A batch is read by a test file running while it was taken, one starting then, and, for a batch
%% taken while no test file ran, by every test file that ended after it.
reads(File, Start, Stop, #{seq := Seq, readers := Readers, gap := Gap}) ->
    lists:member(File, Readers)
        orelse (is_integer(Start) andalso Start =< Seq andalso
                (Stop =:= undefined orelse Seq =< Stop))
        orelse (Gap andalso (Stop =:= undefined orelse Seq =< Stop)).

%% [C-115] What every test file reads: the project's mix.exs and mix.lock, its config folder's
%% files and the test helpers that ran.
every_test(_Graph) ->
    Config = 'Elixir.Mix.Project':config(),
    {ok, Cwd} = file:get_cwd(),
    Root = lookup(root),
    Project = [F || D <- above(list_to_binary(Cwd), Root),
                    F <- [filename:join(D, <<"mix.exs">>)], filelib:is_regular(F)],
    Lock = case proplists:get_value(lockfile, Config) of
               nil -> [];
               L -> [abs(Cwd, L)]
           end,
    ConfigDir = case proplists:get_value(config_path, Config) of
                    nil -> [];
                    C -> [filename:dirname(abs(Cwd, C))]
                end,
    ConfigFiles = [list_to_binary(F) || D <- ConfigDir,
                                       F <- filelib:wildcard(binary_to_list(
                                                               iolist_to_binary([D, "/**/*.exs"])))],
    Helpers = [H || [H] <- ets:match(?TABLE, {{helper, '$1'}, '_'})],
    lists:usort(Project ++ Lock ++ ConfigFiles ++ Helpers).

write_errors(Dir, Errors) ->
    catch write_whole(Dir, io_lib:format("exunit-~s-error.json", [os:getpid()]),
                      encode(#{<<"schema">> => ?SCHEMA, <<"adapter">> => ?WORD,
                               <<"errors">> => Errors})).

write_whole(Dir, Name, Bytes) ->
    Final = filename:join(Dir, Name),
    Temp = filename:join(Dir, "." ++ lists:flatten(Name)),
    ok = file:write_file(Temp, Bytes),
    ok = file:rename(Temp, Final).

note_error(Text) ->
    catch ets:insert(?TABLE, {errors, lists:sublist(lookup(errors) ++ [Text], 20)}),
    ok.

describe(Class, Reason, Stack) ->
    iolist_to_binary(io_lib:format("~p:~0p ~0p", [Class, Reason, lists:sublist(Stack, 3)])).

lookup(Key) ->
    [{_, Value}] = ets:lookup(?TABLE, Key),
    Value.

%% --- paths ---------------------------------------------------------------------------------

abs(Base, File) ->
    normal(iolist_to_binary(filename:absname(File, Base))).

%% A path without `.` or `..`.
normal(Path) ->
    [Top | Parts] = filename:split(Path),
    iolist_to_binary(filename:join([Top | lists:reverse(
                                            lists:foldl(fun(<<".">>, Acc) -> Acc;
                                                           (<<"..">>, [_ | Acc]) -> Acc;
                                                           (<<"..">>, []) -> [];
                                                           (P, Acc) -> [P | Acc]
                                                        end, [], Parts))])).

%% The folders from `Dir` up to the repository's root.
above(Dir, Root) ->
    case inside(Dir, Root) of
        false -> [];
        true when Dir =:= Root -> [Dir];
        true -> [Dir | above(filename:dirname(Dir), Root)]
    end.

inside(Path, Root) ->
    P = iolist_to_binary(Path),
    R = iolist_to_binary(Root),
    P =:= R orelse binary:longest_common_prefix([P, <<R/binary, "/">>]) =:= byte_size(R) + 1.

%% A path from the repository's root, or `none` for one outside it, or in `_build`, `deps` or `.git`.
rel(Root, Path) ->
    P = iolist_to_binary(Path),
    R = iolist_to_binary(Root),
    case inside(P, R) andalso P =/= R of
        false -> none;
        true ->
            Rel = binary:part(P, byte_size(R) + 1, byte_size(P) - byte_size(R) - 1),
            Parts = filename:split(Rel),
            case lists:any(fun(X) -> lists:member(X, Parts) end,
                           [<<"_build">>, <<"deps">>, <<".git">>, <<"..">>, <<".">>]) of
                true -> none;
                false -> Rel
            end
    end.

hex(Bytes) ->
    << <<(hexdigit(N))>> || <<N:4>> <= Bytes >>.

hexdigit(N) when N < 10 -> $0 + N;
hexdigit(N) -> $a + N - 10.

%% --- JSON ----------------------------------------------------------------------------------

decode(Bytes) ->
    {Value, Rest} = value(skip(Bytes)),
    <<>> = skip(Rest),
    Value.

value(<<${, R/binary>>) -> object(skip(R), #{});
value(<<$[, R/binary>>) -> array(skip(R), []);
value(<<$", R/binary>>) -> string(R, []);
value(<<"true", R/binary>>) -> {true, R};
value(<<"false", R/binary>>) -> {false, R};
value(<<"null", R/binary>>) -> {null, R};
value(R) -> number(R).

object(<<$}, R/binary>>, Acc) -> {Acc, R};
object(<<$", R/binary>>, Acc) ->
    {Key, R1} = string(R, []),
    <<$:, R2/binary>> = skip(R1),
    {Value, R3} = value(skip(R2)),
    case skip(R3) of
        <<$,, R4/binary>> -> object(skip(R4), Acc#{Key => Value});
        <<$}, R4/binary>> -> {Acc#{Key => Value}, R4}
    end.

array(<<$], R/binary>>, Acc) -> {lists:reverse(Acc), R};
array(R, Acc) ->
    {Value, R1} = value(R),
    case skip(R1) of
        <<$,, R2/binary>> -> array(skip(R2), [Value | Acc]);
        <<$], R2/binary>> -> {lists:reverse([Value | Acc]), R2}
    end.

string(<<$", R/binary>>, Acc) -> {unicode:characters_to_binary(lists:reverse(Acc)), R};
string(<<$\\, $u, H:4/binary, R/binary>>, Acc) ->
    C = binary_to_integer(H, 16),
    case {C >= 16#D800 andalso C =< 16#DBFF, R} of
        {true, <<$\\, $u, L:4/binary, R1/binary>>} ->
            Low = binary_to_integer(L, 16),
            string(R1, [16#10000 + ((C - 16#D800) bsl 10) + (Low - 16#DC00) | Acc]);
        _ -> string(R, [C | Acc])
    end;
string(<<$\\, E, R/binary>>, Acc) ->
    C = case E of $n -> $\n; $t -> $\t; $r -> $\r; $b -> $\b; $f -> $\f; _ -> E end,
    string(R, [C | Acc]);
string(<<C/utf8, R/binary>>, Acc) -> string(R, [C | Acc]).

number(R) ->
    {Text, Rest} = lists:splitwith(fun(C) -> lists:member(C, "+-0123456789.eE") end,
                                   binary_to_list(R)),
    N = try list_to_integer(Text) catch _:_ -> list_to_float(Text) end,
    {N, list_to_binary(Rest)}.

skip(<<C, R/binary>>) when C =:= $\s; C =:= $\t; C =:= $\n; C =:= $\r -> skip(R);
skip(R) -> R.

encode(true) -> <<"true">>;
encode(false) -> <<"false">>;
encode(null) -> <<"null">>;
encode(N) when is_integer(N) -> integer_to_binary(N);
encode(B) when is_binary(B) -> [$", escape(B), $"];
encode(A) when is_atom(A) -> encode(atom_to_binary(A, utf8));
encode(L) when is_list(L) -> [$[, join([encode(X) || X <- L]), $]];
encode(M) when is_map(M) ->
    [${, join([[encode(K), $:, encode(V)] || {K, V} <- lists:sort(maps:to_list(M))]), $}].

join([]) -> [];
join([X | Xs]) -> [X | [[$,, Y] || Y <- Xs]].

escape(B) ->
    [case C of
         $" -> <<"\\\"">>;
         $\\ -> <<"\\\\">>;
         $\n -> <<"\\n">>;
         $\r -> <<"\\r">>;
         $\t -> <<"\\t">>;
         _ when C < 32 -> io_lib:format("\\u~4.16.0b", [C]);
         _ -> <<C/utf8>>
     end || <<C/utf8>> <= B].
