# frozen_string_literal: true

# hushrun's Ruby adapter (ADR 0114, C-82 to C-85): for each test file RSpec or Minitest runs, what
# it read, and with `unit: test` each test on its own.
#
# hushrun loads this file into every Ruby process of COMMAND through `RUBYOPT=-r<this file>`. In
# a process with no `hushrun.json` in `HUSHRUN_UNITS` it does nothing; in one where neither RSpec
# nor Minitest runs it writes nothing. It requires no gem and no library (it reads and writes its
# JSON itself), so that it never activates a gem before Bundler chooses its version.
#
# "Read" is the spec's meaning: a test file read a file when code of it ran for the test file, it
# was required from code that did, or it was opened, listed, or loaded by code outside the
# repository while the test file ran or before. How it observes:
#
# - Code that ran. Each file of the repository Ruby compiles gets a TracePoint enabled for its
#   code alone (`enable(target: iseq)`): its first line, call or block in a window records the
#   file for that window and disables itself, so each file costs one event per window. Windows are
#   a test file's loading, a test (RSpec's example with its hooks and lets, Minitest's test with
#   setup and teardown), a group's or a class's run around its tests (`before(:context)`), and
#   none (the session). At each window's start and end the files seen are enabled again.
# - Requires. `require`, `require_relative` and `load` from a file of the repository are edges
#   from it to the file they name, followed to the end from every test file: a file another test
#   file loaded first is still on this one's list.
# - Ambient files. A file of the repository loaded by code outside it (Zeitwerk, Rails'
#   initializers, an autoload) or opened by code outside it with no file of the repository on the
#   stack (Rails' fixtures, a template cache) can hold state that later tests use without loading
#   it: it is read by the window running then and every window after it in the process.
# - Opens and listings: `File.open`/`new`, `IO.read`, `binread`, `readlines`, `foreach`,
#   `Dir.glob`, `[]`, `children`, `entries`, `each_child`, `foreach` and `Dir.open`. Every file of
#   the repository on the stack of an open gets an edge to the opened file, so a value a file read
#   once and kept reaches every test that ran its code.
#
# What it can't see it reports (`notFollowed`): a subprocess, and a fork inside a test. A fork with
# no test running (Rails' parallel workers) goes on observing in the child, which writes its own
# facts. Threads that run tests at once make each test's code its process's.
#
# Selection: the first process whose RSpec or Minitest starts claims the run's directory and may
# leave out what `hushrun.json`'s `reuse` and `reuseTests` list: RSpec through an exclusion filter
# (so no hook of a left-out example runs), Minitest by leaving the test out of `runnable_methods`.
#
# It never changes a test's outcome on its own error: each hook is guarded, and an error of its own
# is reported, which makes hushrun record nothing for the test files this process saw.

module HushrunRuby
  SCHEMA = "hushrun.unit-facts/v0"
  SCHEMA_V1 = "hushrun.unit-facts/v1"
  RUN_SCHEMA = "hushrun.units/v0"
  ADAPTER = "ruby"
  CLAIM_FILE = "ruby-claim"
  START_VARIABLE = "HUSHRUN_RUBY_START"
  THIS_FILE = File.expand_path(__FILE__)
  MOST_ERRORS = 20
  GEM_DIR = %r{(\A|/)(vendor/bundle|\.bundle)/|(\A|/)gems/[^/]+-\d[^/]*/}.freeze
  GLOB_META = /[*?\[{]/.freeze

  # [C-32] A small JSON reader and writer, so that no gem is loaded before Bundler's choice.
  module Json
    module_function

    def parse(text)
      @s = text
      @i = 0
      value = read_value
      skip_ws
      raise ArgumentError, "trailing bytes" if @i < @s.bytesize

      value
    end

    def skip_ws
      @i += 1 while @i < @s.bytesize && " \t\r\n".include?(@s.byteslice(@i))
    end

    def read_value
      skip_ws
      c = @s.byteslice(@i)
      case c
      when "{" then read_object
      when "[" then read_array
      when '"' then read_string
      when "t" then literal("true", true)
      when "f" then literal("false", false)
      when "n" then literal("null", nil)
      else read_number
      end
    end

    def literal(word, value)
      raise ArgumentError, "bad literal" unless @s.byteslice(@i, word.size) == word

      @i += word.size
      value
    end

    def read_number
      start = @i
      @i += 1 while @i < @s.bytesize && "+-0123456789.eE".include?(@s.byteslice(@i))
      text = @s.byteslice(start, @i - start)
      raise ArgumentError, "bad value" if text.empty?

      text.match?(/[.eE]/) ? Float(text) : Integer(text, 10)
    end

    def read_object
      @i += 1
      out = {}
      skip_ws
      if @s.byteslice(@i) == "}"
        @i += 1
        return out
      end
      loop do
        skip_ws
        key = read_string
        skip_ws
        raise ArgumentError, "expected :" unless @s.byteslice(@i) == ":"

        @i += 1
        out[key] = read_value
        skip_ws
        c = @s.byteslice(@i)
        @i += 1
        return out if c == "}"
        raise ArgumentError, "expected , or }" unless c == ","
      end
    end

    def read_array
      @i += 1
      out = []
      skip_ws
      if @s.byteslice(@i) == "]"
        @i += 1
        return out
      end
      loop do
        out << read_value
        skip_ws
        c = @s.byteslice(@i)
        @i += 1
        return out if c == "]"
        raise ArgumentError, "expected , or ]" unless c == ","
      end
    end

    ESCAPES = { '"' => '"', "\\" => "\\", "/" => "/", "b" => "\b", "f" => "\f", "n" => "\n",
                "r" => "\r", "t" => "\t" }.freeze

    def read_string
      raise ArgumentError, "expected a string" unless @s.byteslice(@i) == '"'

      @i += 1
      out = +""
      loop do
        c = @s.byteslice(@i)
        raise ArgumentError, "unterminated string" if c.nil?

        @i += 1
        case c
        when '"' then return out.force_encoding(Encoding::UTF_8)
        when "\\"
          e = @s.byteslice(@i)
          @i += 1
          if e == "u"
            code = @s.byteslice(@i, 4).to_i(16)
            @i += 4
            if code.between?(0xD800, 0xDBFF) && @s.byteslice(@i, 2) == "\\u"
              low = @s.byteslice(@i + 2, 4).to_i(16)
              @i += 6
              code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
            end
            out << [code].pack("U")
          else
            out << (ESCAPES[e] || raise(ArgumentError, "bad escape"))
          end
        else
          out << c
        end
      end
    end

    def dump(value, out = +"")
      case value
      when Hash
        out << "{"
        value.keys.map(&:to_s).sort.each_with_index do |key, i|
          out << "," if i.positive?
          string(key, out)
          out << ":"
          dump(value.key?(key) ? value[key] : value[key.to_sym], out)
        end
        out << "}"
      when Array
        out << "["
        value.each_with_index do |item, i|
          out << "," if i.positive?
          dump(item, out)
        end
        out << "]"
      when String, Symbol then string(value.to_s, out)
      when true then out << "true"
      when false then out << "false"
      when nil then out << "null"
      when Integer then out << value.to_s
      when Float then out << (value.finite? ? value.to_s : "null")
      else string(value.to_s, out)
      end
      out
    end

    def string(text, out)
      text = text.dup.force_encoding(Encoding::UTF_8)
      text = text.scrub("\u{FFFD}") unless text.valid_encoding?
      out << '"'
      text.each_char do |ch|
        case ch
        when '"' then out << '\\"'
        when "\\" then out << "\\\\"
        when "\n" then out << "\\n"
        when "\r" then out << "\\r"
        when "\t" then out << "\\t"
        else
          out << (ch.ord < 0x20 ? format("\\u%04x", ch.ord) : ch)
        end
      end
      out << '"'
    end
  end

  # One window: what ran, was opened and listed while it was open.
  class Window
    attr_reader :kind, :unit, :id, :ran, :opened, :dirs, :not_followed
    attr_accessor :start, :stop, :served

    def initialize(kind, unit, id, start)
      @kind = kind
      @unit = unit
      @id = id
      @start = start
      @stop = nil
      @ran = {}
      @opened = {}
      @dirs = {}
      @not_followed = {}
      @served = nil
    end
  end

  # What one test did, in this process.
  class Test
    attr_accessor :unit, :id, :finished, :failed, :skipped, :deselected, :duration, :window,
                  :not_kept

    def initialize(unit, id)
      @unit = unit
      @id = id
      @finished = false
      @failed = false
      @skipped = false
      @deselected = false
      @duration = 0.0
      @window = nil
      @not_kept = nil
    end
  end

  # Everything this process observed.
  class Observer
    attr_reader :root, :directory, :errors, :start_id
    attr_accessor :framework, :claimed, :live

    def initialize(root, directory, run)
      @root = root
      @roots = [root, (File.realpath(root) rescue root)].uniq
      @directory = directory
      @reuse = {}
      (run["reuse"] || []).each { |u| @reuse[u] = true if u.is_a?(String) }
      @by_test = run["unit"] == "test"
      @wants_watch = @by_test && run["reads"] == "test"
      @reuse_tests = {}
      if @by_test && run["reuseTests"].is_a?(Hash)
        run["reuseTests"].each do |unit, ids|
          next unless ids.is_a?(Array)

          set = {}
          ids.each { |id| set[id] = true if id.is_a?(String) }
          @reuse_tests[unit] = set
        end
      end
      @errors = []
      @pid = Process.pid
      @start_id = format("%d-%d-%d", Process.pid, (Time.now.to_f * 1000).to_i, rand(1 << 30))
      @framework = nil
      @versions = {}
      @claimed = false
      @live = true
      @inert = false
      @lock = Mutex.new
      @rel_cache = {}
      @seq = 0
      @stack = []
      @windows = []
      @session = Window.new(:session, nil, nil, 0)
      @ambient = []                    # [seq, rel, :ran | :opened]
      @edges = Hash.new { |h, k| h[k] = {} }       # rel -> {rel => true}: requires
      @open_edges = Hash.new { |h, k| h[k] = {} }  # rel -> {rel => true}: opens
      @list_edges = Hash.new { |h, k| h[k] = {} }  # rel -> {dir => true}: listings
      @autoloads = {}
      @tracepoints = {}                # rel -> [tp, iseq]
      @fired = []
      @watching = true
      @watch_reason = nil
      @concurrent = false
      @units = {}                      # rel -> { selected:, ... }
      @unit_files = {}
      @tests = {}                      # [unit, id] -> Test
      @test_order = []
      @deselected_units = {}
      @removed = 0
      @loaded = {}                     # rel -> true: every repository file compiled here
      @compiled_seen = {}
      @main = nil
      @failure_seen = false
      @started_file = nil
      @done = false
      @run_started = false
    end

    def live?
      @live && !@inert && Process.pid == @pid
    end

    def by_test?
      @by_test
    end

    def guard(where)
      yield
    rescue Exception => e # rubocop:disable Lint/RescueException
      raise if e.is_a?(SystemExit) || e.is_a?(Interrupt) || e.is_a?(SignalException)

      @errors << "#{where}: #{e.class}: #{e.message}"[0, 300] if @errors.size < MOST_ERRORS
      nil
    end

    # --- paths -----------------------------------------------------------------------------

    # `path` from the repository's root, when it is a file of the repository outside an installed
    # gem; else nil.
    def rel(path)
      return nil if path.nil?

      path = path.to_path if path.respond_to?(:to_path)
      return nil unless path.is_a?(String)

      cached = @rel_cache[path]
      return cached || nil unless cached.nil?

      found = compute_rel(path)
      @rel_cache[path] = found || false
      found
    end

    def compute_rel(path)
      return nil if path.start_with?("<") || path == THIS_FILE

      abs = File.expand_path(path)
      real = begin
        File.realpath(abs)
      rescue StandardError
        nil
      end
      [abs, real].compact.uniq.each do |candidate|
        @roots.each do |root|
          next unless candidate.start_with?(root + "/")

          inside = candidate[(root.size + 1)..]
          return nil if inside.match?(GEM_DIR)

          return inside
        end
      end
      nil
    end

    def repo_frames(skip)
      out = []
      locations = caller_locations(skip)
      return out unless locations

      locations.each do |loc|
        r = rel(loc.absolute_path || loc.path)
        out << r if r && !out.include?(r)
      end
      out
    end

    # --- windows ---------------------------------------------------------------------------

    def current
      @stack.last || @session
    end

    def rearm
      return unless @watching

      fired = @fired
      @fired = []
      fired.each do |tp, iseq|
        tp.enable(target: iseq) unless tp.enabled?
      rescue StandardError => e
        stop_watching("a TracePoint couldn't be enabled again (#{e.class})")
        return
      end
    end

    def open_window(kind, unit, id)
      @lock.synchronize do
        if kind == :test && @stack.any? { |w| w.kind == :test }
          # Tests that run at once in threads: no test's code can be told apart.
          @concurrent = true
        end
        @seq += 1
        rearm
        w = Window.new(kind, unit, id, @seq)
        @stack.push(w)
        @windows << w
        w
      end
    end

    def close_window(window)
      @lock.synchronize do
        @seq += 1
        window.stop = @seq
        @stack.delete(window)
        rearm
      end
    end

    def window(kind, unit, id)
      w = open_window(kind, unit, id)
      begin
        yield w
      ensure
        close_window(w)
      end
    end

    # --- watching --------------------------------------------------------------------------

    def compiled(iseq, eval_script)
      return if eval_script

      path = iseq.absolute_path || iseq.path
      r = rel(path)
      return unless r

      @loaded[r] = true
      @compiled_seen[r] = true
      # The main script is a test file's loading, open until its runner starts.
      if @stack.empty? && @main.nil? && File.expand_path(path) == File.expand_path($PROGRAM_NAME)
        @main = open_window(:load, r, nil)
      end
      w = current
      unless explicitly_loaded? || @stack.empty? || (w.kind == :load && w.unit == r)
        # [C-83] Loaded by code outside the repository: what it holds can reach later tests.
        @ambient << [w.start, r, :ran]
      end
      return unless @watching

      tp = TracePoint.new(:line, :call, :b_call) do |event|
        HushrunRuby.hit(r, event)
      end
      begin
        tp.enable(target: iseq)
      rescue ArgumentError
        # A file of comments alone has no line, call or block to watch: none of it can run.
        return
      end
      @tracepoints[r] = [tp, iseq]
    rescue StandardError => e
      stop_watching("a TracePoint couldn't watch #{r} (#{e.class})")
    end

    def hit(rel, tp)
      w = current
      w.ran[rel] = true
      entry = @tracepoints[rel]
      @fired << entry if entry
      tp.disable
    rescue StandardError
      nil
    end

    def stop_watching(why)
      return unless @watching

      @watching = false
      @watch_reason ||= why
      @tracepoints.each_value do |tp, _|
        tp.disable if tp.enabled?
      rescue StandardError
        nil
      end
      @fired = []
    end

    def watching?
      @watching
    end

    # --- requires --------------------------------------------------------------------------

    def explicitly_loaded?
      stack = Thread.current[:hushrun_ruby_requires]
      stack && !stack.empty? && stack.last
    end

    def autoload_registered(path)
      p = path.respond_to?(:to_path) ? path.to_path : path
      return unless p.is_a?(String)

      @autoloads[p] = true
      @autoloads[File.expand_path(p)] = true if p.start_with?("/", ".")
    end

    # The file `kind` (`:require`, `:load`) of `feature` from `from` names, when it can tell
    # without searching for a feature only code outside the repository asked for.
    def resolve(kind, feature, from_repo)
      feature = feature.to_path if feature.respond_to?(:to_path)
      return nil unless feature.is_a?(String)

      if kind == :load
        return File.expand_path(feature) if File.file?(feature)
        return nil unless from_repo

        $LOAD_PATH.each do |dir|
          candidate = File.join(dir.to_s, feature)
          return File.expand_path(candidate) if File.file?(candidate)
        end
        return nil
      end
      if feature.start_with?("/")
        return feature if File.file?(feature)
        return "#{feature}.rb" if File.file?("#{feature}.rb")

        return nil
      end
      return nil unless from_repo

      found = $LOAD_PATH.resolve_feature_path(feature)
      found && found[0] == :rb ? found[1] : nil
    rescue LoadError, StandardError
      nil
    end

    # Whether the line that called is a require, a require_relative or a load: one that names
    # none is Ruby loading an autoload a constant there named.
    def asks_for_it?(path, line)
      return true if line.nil?

      lines = (@source_lines ||= {})[path] ||= begin
        Thread.current[:hushrun_ruby_quiet] = true
        File.readlines(path)
      rescue StandardError
        []
      ensure
        Thread.current[:hushrun_ruby_quiet] = nil
      end
      text = lines[line - 1]
      text.nil? || text.match?(/\b(require|require_relative|load)\b/)
    end

    def around_require(kind, feature, caller_path, caller_line = nil)
      unless live?
        return yield
      end

      from = rel(caller_path)
      key = feature.respond_to?(:to_path) ? feature.to_path : feature
      explicit = !from.nil? && !(key.is_a?(String) && @autoloads.key?(key)) &&
                 asks_for_it?(caller_path, caller_line)
      target = resolve(kind, feature, !from.nil? || @stack.empty? || @stack.last.equal?(@main))
      target_rel = rel(target)
      outer = target_rel && !@run_started &&
              (@stack.empty? || (@stack.size == 1 && @stack[0].equal?(@main)))
      stack = (Thread.current[:hushrun_ruby_requires] ||= [])
      stack.push(explicit)
      w = outer ? open_window(:load, target_rel, nil) : nil
      begin
        result = yield
      ensure
        stack.pop
        close_window(w) if w
        guard("require") do
          @edges[from][target_rel] = true if explicit && target_rel && target_rel != from
          HushrunRuby.framework_check
        end
      end
      if target_rel && (result == true || kind == :load)
        @loaded[target_rel] = true
        unless @compiled_seen.key?(target_rel)
          # [C-84] A compile cache (Bootsnap's) loaded it without Ruby compiling it here.
          stop_watching("#{target_rel} was loaded without Ruby compiling it (a compile cache " \
                        "such as Bootsnap's), so the code each test ran can't be watched")
        end
      end
      result
    end

    # --- opens, listings, subprocesses -----------------------------------------------------

    def opened(path)
      return unless live?
      return if Thread.current[:hushrun_ruby_quiet]
      return if path.is_a?(Integer) || path.is_a?(IO)

      path = path.to_path if path.respond_to?(:to_path)
      return unless path.is_a?(String)
      return if path.start_with?("|")

      r = rel(path)
      return unless r

      frames = repo_frames(2)
      w = current
      w.opened[r] = true
      if frames.empty?
        @ambient << [w.start, r, :opened] unless @stack.empty?
      else
        frames.each { |f| @open_edges[f][r] = true if f != r }
      end
    end

    # [C-83] A directory listed by code of the repository, read by the window open then and, by an
    # edge from that file, by every test file that runs it. One a gem's code lists (Zeitwerk
    # setting up its autoloads, Active Record finding fixtures, the runner finding its test files)
    # is no read: what of it a test uses, it loads or opens.
    def listed(path, glob)
      return unless live?

      loc = caller_locations(2, 8)&.find do |l|
        p = l.absolute_path || l.path
        p != THIS_FILE && !p.start_with?("<internal:")
      end
      from = loc && rel(loc.absolute_path || loc.path)
      return if from.nil?

      path = path.to_path if path.respond_to?(:to_path)
      return unless path.is_a?(String)

      if glob
        at = path =~ GLOB_META
        if at
          base = path[0, at]
          path = base.end_with?("/") || base.empty? ? base : File.dirname(base)
          path = "." if path.empty?
        end
      end
      abs = File.expand_path(path)
      r = if @roots.any? { |root| abs == root || (File.realpath(abs) rescue nil) == root }
            ""
          else
            rel(abs)
          end
      return if r.nil?

      current.dirs[r] = true
      @list_edges[from][r] = true
    end

    def not_followed(why, detail)
      return unless live?

      (current.not_followed[why] ||= {})[detail.to_s[0, 200]] = true
    end

    def forked_child
      if @stack.any? { |w| w.kind == :test || w.kind == :group }
        # A fork inside a test: the child is the test's doing, and writes nothing.
        @inert = true
        return
      end
      @pid = Process.pid
      @started_file = nil
      @claimed = false
    end

    def forked_parent
      return unless live?

      not_followed("fork", "a child process the test forked") if @stack.any? { |w| w.kind != :load }
    end

    # --- tests -----------------------------------------------------------------------------

    def unit_entry(unit)
      @units[unit] ||= { selected: 0, finished: 0, failed: false, duration: 0.0, skipped: 0 }
    end

    def list_unit(unit)
      @unit_files[unit] = true
    end

    def may_deselect?
      @claimed && (!@reuse.empty? || !@reuse_tests.empty?)
    end

    def leave_out?(unit, id)
      return false unless may_deselect?
      return true if @reuse.key?(unit)

      !id.nil? && @reuse_tests.key?(unit) && @reuse_tests[unit].key?(id)
    end

    def test(unit, id)
      key = [unit, id]
      found = @tests[key]
      return found if found

      t = Test.new(unit, id)
      @tests[key] = t
      @test_order << key
      t
    end

    def seen?(unit, id)
      !@tests[[unit, id]]&.window.nil?
    end

    def selected(unit, count)
      unit_entry(unit)[:selected] += count
    end

    def deselected(unit, id)
      @removed += 1
      t = test(unit, id)
      t.deselected = true
      @deselected_units[unit] = true
    end

    def ran_test(unit, id)
      t = test(unit, id)
      t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      outcome = nil
      window(:test, unit, id) do |w|
        t.window = w
        outcome = yield
      end
      t.duration += Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0
      outcome
    end

    # How a test ended: `status` `:passed`, `:failed` or `:skipped`.
    def ended(unit, id, status)
      t = test(unit, id)
      t.finished = true
      entry = unit_entry(unit)
      entry[:finished] += 1
      entry[:duration] += t.duration
      case status
      when :failed
        t.failed = true
        entry[:failed] = true
        @failure_seen = true
      when :skipped
        t.skipped = true
        entry[:skipped] += 1
      end
    end

    def unit_failed(unit)
      unit_entry(unit)[:failed] = true
      @failure_seen = true
    end

    def run_started
      return if @run_started

      @run_started = true
      return unless @main

      close_window(@main)
    end

    # What runs as the process ends, before its facts are made: the framework's own listing.
    def at_end(&block)
      (@at_end ||= []) << block
    end

    def note_version(name, version)
      @versions[name] = version
    end

    # --- the end ---------------------------------------------------------------------------

    def closure(roots, edges)
      seen = {}
      pending = roots.dup
      until pending.empty?
        f = pending.pop
        next if seen.key?(f)

        seen[f] = true
        edges[f]&.each_key { |t| pending << t }
      end
      seen
    end

    def file_edges
      out = Hash.new { |h, k| h[k] = {} }
      [@edges, @open_edges].each do |table|
        table.each { |f, targets| out[f].merge!(targets) }
      end
      out
    end

    def windows_of(unit)
      @windows.select { |w| w.unit == unit }
    end

    def results
      edges = file_edges
      units = (@units.keys + @unit_files.keys + @tests.keys.map(&:first)).uniq.sort
      unit_set = {}
      units.each { |u| unit_set[u] = true }
      # What every test of the process reads: what ran outside every window, and the loading of
      # files that are no test file (spec_helper, the app's boot), and [C-83] with threads that ran
      # tests at once, what each ran.
      session = Window.new(:session, nil, nil, 0)
      @windows.each do |w|
        next unless w.equal?(@session) || !unit_set.key?(w.unit) ||
                    (@concurrent && (w.kind == :test || w.kind == :group))

        merge_window(session, w)
      end
      merge_window(session, @session)
      @session_all = session
      units.filter_map do |unit|
        info = @units[unit] || unit_entry(unit)
        deselected = @deselected_units.key?(unit) && info[:selected].zero?
        # A test file no test of which was selected, here or by hushrun, says nothing.
        next if info[:selected].zero? && !deselected && info[:finished].zero? && !info[:failed]

        own = windows_of(unit)
        last = own.map { |w| w.stop || @seq }.max || 0
        roots = { unit => true }
        not_followed = {}
        dirs = {}
        ([session] + own).each do |w|
          roots.merge!(w.ran)
          roots.merge!(w.opened)
          dirs.merge!(w.dirs)
          w.not_followed.each { |k, v| (not_followed[k] ||= {}).merge!(v) }
        end
        @ambient.each { |seq, r, _| roots[r] = true if seq <= last }
        roots.merge!(@loaded) unless @watching
        reads = closure(roots.keys, edges)
        reads.each_key { |f| @list_edges[f]&.each_key { |d| dirs[d] = true } if @list_edges.key?(f) }
        entry = {
          "unit" => unit,
          "selected" => info[:selected],
          "finished" => info[:finished],
          "failed" => info[:failed],
          "deselected" => deselected,
          "durationMs" => (info[:duration] * 1000).round,
          "skipped" => info[:skipped],
          "reads" => reads.keys.sort,
          "dirs" => dirs.keys.sort,
          "notFollowed" => not_followed.sort.flat_map do |why, details|
            details.keys.sort.map { |d| { "why" => why, "detail" => d } }
          end,
        }
        if @by_test
          entry["tests"] = test_results(unit)
          watch_results(unit, entry, own) if watched?
        end
        entry
      end
    end

    def merge_window(into, w)
      into.ran.merge!(w.ran)
      into.opened.merge!(w.opened)
      into.dirs.merge!(w.dirs)
      w.not_followed.each { |k, v| (into.not_followed[k] ||= {}).merge!(v) }
    end

    def watched?
      @wants_watch && @watching && !@concurrent
    end

    def test_results(unit)
      @test_order.select { |u, _| u == unit }.map do |key|
        t = @tests[key]
        out = {
          "id" => t.id,
          "finished" => t.finished,
          "failed" => t.failed,
          "deselected" => t.deselected,
          "durationMs" => (t.duration * 1000).round,
        }
        out["skipped"] = true if t.skipped && !t.deselected
        out["notKept"] = t.not_kept if t.not_kept
        if t.window && !t.window.not_followed.empty?
          out["notFollowed"] = t.window.not_followed.sort.flat_map do |why, details|
            details.keys.sort.map { |d| { "why" => why, "detail" => d } }
          end
        end
        out
      end
    end

    def paths_index(path)
      @path_index ||= {}
      @paths ||= []
      at = @path_index[path]
      unless at
        at = @paths.size
        @paths << path
        @path_index[path] = at
      end
      at
    end

    def indices(paths)
      paths.map { |p| paths_index(p) }.uniq.sort
    end

    # [ADR 0114] What each test of `unit` touched, what loading it did, and its groups' setups,
    # by index into the file's `paths`.
    def watch_results(unit, entry, own)
      session = @session_all
      tests = entry["tests"]
      load_windows = own.select { |w| w.kind == :load }
      loaded_ran = {}
      loaded_opened = {}
      load_windows.each do |w|
        loaded_ran.merge!(w.ran)
        loaded_opened.merge!(w.opened)
      end
      tests.each do |test|
        t = @tests[[unit, test["id"]]]
        w = t&.window
        next unless w

        ran = w.ran.dup
        opened = w.opened.dup
        dirs = w.dirs.dup
        ran.merge!(session.ran)
        opened.merge!(session.opened)
        dirs.merge!(session.dirs)
        stop = w.stop || @seq
        @ambient.each do |seq, r, how|
          next if seq > stop

          (how == :ran ? ran : opened)[r] = true
        end
        test["ran"] = indices(ran.keys)
        test["opened"] = indices(opened.keys)
        test["dirs"] = indices(dirs.keys)
      end
      entry["loaded"] = { "ran" => indices(loaded_ran.keys), "opened" => indices(loaded_opened.keys) }
      # [C-83] Every test of the file reads what it and the session's files require, to the end:
      # a constant a required file defines is read without its code running.
      every = closure([unit] + session.ran.keys, @edges)
      entry["everyTest"] = indices(every.keys)
      shared = []
      order = tests.map { |t| t["id"] }
      own.select { |w| w.kind == :group }.each do |g|
        served = []
        order.each_with_index do |id, at|
          tw = @tests[[unit, id]]&.window
          next unless tw
          next unless g.served ? g.served.include?(id) : (tw.start >= g.start && tw.start <= (g.stop || @seq))

          served << at
        end
        next if served.empty?

        shared << { "tests" => served, "ran" => indices(g.ran.keys), "opened" => indices(g.opened.keys) }
      end
      entry["shared"] = shared
    end

    def watch_tables
      imports = {}
      @edges.each do |f, targets|
        t = targets.keys.reject { |x| x == f }
        imports[paths_index(f).to_s] = indices(t) unless t.empty?
      end
      opens = {}
      @open_edges.each do |f, targets|
        t = targets.keys.reject { |x| x == f }
        opens[paths_index(f).to_s] = indices(t) unless t.empty?
      end
      [imports, opens]
    end

    def edge_list
      out = []
      @edges.each { |f, ts| ts.each_key { |t| out << [f, t, "import"] if t != f } }
      @open_edges.each { |f, ts| ts.each_key { |t| out << [f, t, "open"] if t != f } }
      out.uniq.sort
    end

    def begin_start
      path = File.join(@directory, "ruby-#{Process.pid}.started")
      File.write(path, Json.dump({ "why" => "a Ruby process running #{@framework} ended before it said what it saw" }))
      @started_file = path
    rescue StandardError => e
      @errors << "started: #{e.message}"[0, 300]
    end

    def finish(exiting_with)
      return if @done || !@framework || @inert || Process.pid != @pid

      @done = true
      (@at_end || []).each { |block| guard("the end") { block.call } }
      @live = false
      @tracepoints.each_value do |tp, _|
        tp.disable if tp.enabled?
      rescue StandardError
        nil
      end
      units = begin
        results
      rescue StandardError => e
        @errors << "results: #{e.class}: #{e.message}"[0, 300]
        []
      end
      failed_status = !exiting_with.nil? && exiting_with != 0
      settings = {
        "ruby" => RUBY_VERSION,
        "framework" => @framework,
        "testFiles" => @unit_files.keys.sort,
      }
      @versions.each { |k, v| settings[k] = v }
      settings["watch"] = watch_reason if @wants_watch && !watched?
      facts = {
        "schema" => @by_test ? SCHEMA_V1 : SCHEMA,
        "adapter" => ADAPTER,
        "start" => @start_id,
        "pid" => Process.pid,
        "controller" => false,
        # [C-32] A process that failed with no test of it failing failed outside every test (a
        # `before(:suite)`, an `after(:context)`, a test file that didn't load): no pass of it is
        # kept.
        "failuresByUnit" => !(failed_status && !@failure_seen),
        "settings" => settings,
        "notFollowed" => [],
        "units" => units,
        "edges" => (edge_list rescue []),
        "errors" => @errors,
        "removedItems" => @removed,
      }
      if failed_status && !@failure_seen
        facts["notKept"] = "#{@framework} failed outside every test (an error in a hook of the whole " \
                           "run or of a group, or a test file that didn't load), so no pass of the run is kept"
      end
      if @by_test && watched?
        begin
          imports, opens = watch_tables
          facts["imports"] = imports
          facts["opens"] = opens
          facts["paths"] = @paths || []
        rescue StandardError => e
          @errors << "watch tables: #{e.message}"[0, 300]
          units.each { |u| u.delete("loaded") }
        end
      end
      name = "ruby-#{Process.pid}.json"
      tmp = File.join(@directory, ".#{name}.tmp")
      File.binwrite(tmp, Json.dump(facts))
      File.rename(tmp, File.join(@directory, name))
      File.delete(@started_file) if @started_file && File.exist?(@started_file)
    rescue StandardError
      nil
    end

    def watch_reason
      return "tests ran at once in threads, so no test's code can be told apart" if @concurrent

      @watch_reason || "watching was off"
    end

    def claim
      path = File.join(@directory, CLAIM_FILE)
      File.open(path, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |f| f.write(Process.pid.to_s) }
      @claimed = true
    rescue StandardError
      @claimed = false
    end
  end

  class << self
    attr_reader :observer

    def hit(rel, tp)
      @observer&.hit(rel, tp)
    end

    def obs
      o = @observer
      o&.live? ? o : nil
    end

    # Called after each require: installs the hooks of a framework that just loaded.
    def framework_check
      o = obs
      return unless o

      if !@rspec && defined?(::RSpec::Core::Example) && defined?(::RSpec::Core::World) &&
         defined?(::RSpec::Core::Configuration) && defined?(::RSpec::Core::ExampleGroup)
        @rspec = true
        o.guard("rspec") { RSpecHooks.install(o) }
      end
      if !@minitest && defined?(::Minitest::Runnable) && defined?(::Minitest::Test) &&
         ::Minitest::Test.method_defined?(:run)
        @minitest = true
        o.guard("minitest") { MinitestHooks.install(o) }
      end
    end

    # The first framework of the process: it claims the run when no other process did.
    def activate(o, name)
      return if o.framework

      o.framework = name
      ENV[START_VARIABLE] = o.start_id
      o.claim
      o.begin_start
    end

    def start
      directory = ENV["HUSHRUN_UNITS"]
      return if directory.nil? || directory.empty?
      return unless RUBY_ENGINE == "ruby"
      # A Ruby a test started inherits this: it is the test's doing.
      return if ENV[START_VARIABLE] && !ENV[START_VARIABLE].empty?

      text = File.binread(File.join(directory, "hushrun.json"))
      run = Json.parse(text.force_encoding(Encoding::UTF_8))
      return unless run.is_a?(Hash) && run["schema"] == RUN_SCHEMA && run["root"].is_a?(String)

      if (RUBY_VERSION.split(".").map(&:to_i) <=> [3, 1]).negative?
        return
      end

      @observer = Observer.new(File.expand_path(run["root"]), directory, run)
      install
      at_exit do
        status = $!.is_a?(SystemExit) ? $!.status : ($! ? 1 : 0)
        @observer.finish(status)
      end
    rescue StandardError
      @observer = nil
    end

    def install
      TracePoint.new(:script_compiled) do |tp|
        o = obs
        o&.compiled(tp.instruction_sequence, tp.eval_script)
      end.enable
      wrap_kernel
      ::Module.prepend(ModuleHooks)
      ::File.prepend(FileHooks)
      ::IO.singleton_class.prepend(IOClassHooks)
      ::Dir.singleton_class.prepend(DirClassHooks)
      ::Dir.prepend(DirHooks)
      ::Process.singleton_class.prepend(ProcessHooks)
      ::IO.singleton_class.prepend(PopenHooks)
      framework_check
    end
  end

  # require, require_relative and load, and the processes Kernel starts.
  # require, require_relative and load, and the processes Kernel starts: wrapped in Kernel itself,
  # as RubyGems, Zeitwerk and Bootsnap wrap them, so that a `module_function` one of them makes
  # later copies this one with the rest.
  def self.wrap_kernel
    # [C-83] Ruby 3.3's bundled_gems.rb sends every require to `Kernel.require`, the copy
    # `module_function` made: that copy is wrapped too.
    ::Kernel.singleton_class.class_eval do
      alias_method :hushrun_ruby_require, :require
      alias_method :hushrun_ruby_load, :load

      def require(feature)
        o = HushrunRuby.obs
        return hushrun_ruby_require(feature) unless o

        loc = HushrunRuby.caller_of_require
        o.around_require(:require, feature, loc&.absolute_path || loc&.path, loc&.lineno) do
          hushrun_ruby_require(feature)
        end
      end

      def load(file, wrap = false)
        o = HushrunRuby.obs
        return hushrun_ruby_load(file, wrap) unless o

        loc = HushrunRuby.caller_of_require
        o.around_require(:load, file, loc&.absolute_path || loc&.path, loc&.lineno) do
          hushrun_ruby_load(file, wrap)
        end
      end
    end
    ::Kernel.module_eval do
      alias_method :hushrun_ruby_require, :require
      alias_method :hushrun_ruby_load, :load
      alias_method :hushrun_ruby_system, :system
      alias_method :hushrun_ruby_spawn, :spawn
      alias_method :hushrun_ruby_backtick, :`

      def require(feature)
        o = HushrunRuby.obs
        return hushrun_ruby_require(feature) unless o

        loc = HushrunRuby.caller_of_require
        o.around_require(:require, feature, loc&.absolute_path || loc&.path, loc&.lineno) do
          hushrun_ruby_require(feature)
        end
      end

      # Ruby's own resolves the path from the file that calls it, which a method of this file
      # would be: so it is done here as Ruby does it, a require of the absolute path from that
      # file's directory, through the require Ruby had before RubyGems wrapped it.
      def require_relative(feature)
        loc = caller_locations(1, 1)&.first
        base = loc&.absolute_path
        raise LoadError, "cannot infer basepath" if base.nil?

        feature = feature.to_path if feature.respond_to?(:to_path)
        path = File.expand_path(feature.to_str, File.dirname(base))
        original = HushrunRuby.original_require
        o = HushrunRuby.obs
        return original.bind_call(self, path) if o.nil? || o.rel(base).nil?

        o.around_require(:require, path, base, loc.lineno) { original.bind_call(self, path) }
      end

      def load(file, wrap = false)
        o = HushrunRuby.obs
        return hushrun_ruby_load(file, wrap) unless o

        loc = HushrunRuby.caller_of_require
        o.around_require(:load, file, loc&.absolute_path || loc&.path, loc&.lineno) do
          hushrun_ruby_load(file, wrap)
        end
      end

      def system(*args, **kw)
        HushrunRuby.obs&.not_followed("subprocess", HushrunRuby.command_word(args))
        hushrun_ruby_system(*args, **kw)
      end

      def spawn(*args, **kw)
        HushrunRuby.obs&.not_followed("subprocess", HushrunRuby.command_word(args))
        hushrun_ruby_spawn(*args, **kw)
      end

      def `(command)
        HushrunRuby.obs&.not_followed("subprocess", HushrunRuby.command_word([command]))
        hushrun_ruby_backtick(command)
      end

      private :require, :require_relative, :load, :system, :spawn, :`,
              :hushrun_ruby_require, :hushrun_ruby_load, :hushrun_ruby_system,
              :hushrun_ruby_spawn, :hushrun_ruby_backtick
    end
  end

  module ModuleHooks
    def autoload(const, path)
      HushrunRuby.observer&.autoload_registered(path)
      super
    end
  end

  module FileHooks
    def initialize(path, *args, **kw, &block)
      HushrunRuby.obs&.opened(path)
      super
    end
  end

  module IOClassHooks
    %i[read binread readlines foreach].each do |name|
      define_method(name) do |path, *args, **kw, &block|
        HushrunRuby.obs&.opened(path)
        super(path, *args, **kw, &block)
      end
    end
  end

  module PopenHooks
    def popen(*args, **kw, &block)
      HushrunRuby.obs&.not_followed("subprocess", HushrunRuby.command_word(args))
      super
    end
  end

  module DirClassHooks
    def glob(pattern, *args, **kw, &block)
      o = HushrunRuby.obs
      if o
        base = kw[:base]
        Array(pattern).each do |p|
          p = p.to_path if p.respond_to?(:to_path)
          next unless p.is_a?(String)

          o.listed(base && !p.start_with?("/") ? File.join(base.to_s, p) : p, true)
        end
      end
      super
    end

    def [](*patterns, **kw)
      o = HushrunRuby.obs
      if o
        base = kw[:base]
        patterns.each do |p|
          p = p.to_path if p.respond_to?(:to_path)
          next unless p.is_a?(String)

          o.listed(base && !p.start_with?("/") ? File.join(base.to_s, p) : p, true)
        end
      end
      super
    end

    %i[children entries each_child foreach empty?].each do |name|
      define_method(name) do |path, *args, **kw, &block|
        HushrunRuby.obs&.listed(path, false)
        super(path, *args, **kw, &block)
      end
    end
  end

  module DirHooks
    def initialize(path, *args, **kw)
      HushrunRuby.obs&.listed(path, false)
      super
    end
  end

  module ProcessHooks
    def _fork
      o = HushrunRuby.observer
      pid = super
      if o
        if pid.zero?
          o.forked_child
        else
          o.forked_parent
        end
      end
      pid
    end

    def spawn(*args, **kw)
      HushrunRuby.obs&.not_followed("subprocess", HushrunRuby.command_word(args))
      super
    end
  end

  # Files whose frames only pass a require on: RubyGems', Ruby's bundled_gems.rb, Zeitwerk's and
  # Bootsnap's wrappers, and this one.
  REQUIRE_WRAPPERS = %r{(\A<internal:|/bundled_gems\.rb\z|/kernel_require\.rb\z|/core_ext/kernel\.rb\z)}.freeze

  # The frame that asked for a require: the first above every wrapper's.
  def self.caller_of_require
    caller_locations(2, 12)&.find do |loc|
      path = loc.absolute_path || loc.path
      path != THIS_FILE && !path.match?(REQUIRE_WRAPPERS)
    end
  end

  # The require Ruby had before RubyGems wrapped it, which Ruby's require_relative calls.
  def self.original_require
    @original_require ||= if ::Kernel.private_method_defined?(:gem_original_require)
                            ::Kernel.instance_method(:gem_original_require)
                          else
                            ::Kernel.instance_method(:hushrun_ruby_require)
                          end
  end

  def self.command_word(args)
    first = args.find { |a| a.is_a?(String) || a.is_a?(Array) }
    first = first.first if first.is_a?(Array)
    first.to_s.split(/\s+/).find { |w| !w.include?("=") }.to_s[0, 200]
  end

  # [ADR 0114] RSpec 3: spec files load in windows of their own, each example runs in one, a
  # group's run around its examples is a shared setup, and left-out examples are excluded before
  # any hook runs.
  module RSpecHooks
    module_function

    # What the exclusion filter says, which RSpec prints.
    REUSED = "already passed on the files it read"

    def install(o)
      HushrunRuby.activate(o, "rspec")
      o.note_version("rspec", ::RSpec::Core::Version::STRING) if defined?(::RSpec::Core::Version)
      ::RSpec::Core::Configuration.prepend(ConfigurationHooks)
      ::RSpec::Core::World.prepend(WorldHooks)
      ::RSpec::Core::Example.prepend(ExampleHooks)
      ::RSpec::Core::ExampleGroup.singleton_class.prepend(GroupHooks)
    end

    def unit_of(metadata)
      file = metadata[:rerun_file_path] || metadata[:file_path]
      file && HushrunRuby.observer.rel(file)
    end

    def id_of(metadata)
      scoped = metadata[:scoped_id]
      scoped ? "[#{scoped}]" : nil
    end

    module ConfigurationHooks
      def load_spec_files
        o = HushrunRuby.obs
        if o
          o.guard("rspec files") do
            files_to_run.each do |f|
              r = o.rel(f)
              o.list_unit(r) if r
            end
          end
        end
        super
      end
    end

    module WorldHooks
      def announce_filters
        o = HushrunRuby.obs
        o&.run_started
        if o&.may_deselect?
          o.guard("rspec filter") do
            # An exclusion filter matches what an example's own metadata says: each example
            # hushrun.json lists says so, and RSpec leaves it out before any of its hooks runs.
            marked = false
            example_groups.each do |top|
              [top, *top.descendants].uniq.each do |group|
                group.examples.each do |ex|
                  unit = RSpecHooks.unit_of(ex.metadata)
                  next unless unit && o.leave_out?(unit, RSpecHooks.id_of(ex.metadata))

                  ex.metadata[:hushrun_reused] = RSpecHooks::REUSED
                  marked = true
                end
              end
            end
            ::RSpec.configuration.filter_run_excluding(hushrun_reused: RSpecHooks::REUSED) if marked
          end
        end
        result = super
        o&.guard("rspec selection") { RSpecHooks.count(o, self) }
        result
      end
    end

    # What the filters left of every group's examples, and what this process left out.
    def count(o, world)
      world.example_groups.each do |top|
        [top, *top.descendants].uniq.each do |group|
          kept = world.filtered_examples[group]
          group.examples.each do |ex|
            unit = unit_of(ex.metadata)
            next unless unit

            if kept.include?(ex)
              o.selected(unit, 1)
              o.test(unit, id_of(ex.metadata)) if o.by_test?
            elsif o.leave_out?(unit, id_of(ex.metadata))
              # Left out by hushrun's filter alone: what another filter left out stays unrun.
              o.deselected(unit, id_of(ex.metadata)) unless others_exclude?(ex)
            end
          end
        end
      end
    end

    def others_exclude?(example)
      manager = ::RSpec.configuration.filter_manager
      without = manager.exclusions.rules.reject { |k, _| k == :hushrun_reused }
      return true if without.any? do |k, v|
        ::RSpec::Core::MetadataFilter.filter_applies?(k, v, example.metadata)
      end

      inclusions = manager.inclusions
      !inclusions.empty? && !inclusions.include_example?(example)
    rescue StandardError
      true
    end

    module ExampleHooks
      def run(example_group_instance, reporter)
        o = HushrunRuby.obs
        unit = o && RSpecHooks.unit_of(metadata)
        return super unless unit

        id = RSpecHooks.id_of(metadata)
        result = o.ran_test(unit, id) { super }
        o.guard("rspec example") do
          status = execution_result.status
          o.ended(unit, id, if status == :pending
                              :skipped
                            elsif status == :passed
                              :passed
                            else
                              :failed
                            end)
        end
        result
      end
    end

    module GroupHooks
      def run(reporter = ::RSpec::Core::NullReporter)
        o = HushrunRuby.obs
        unit = o && RSpecHooks.unit_of(metadata)
        return super unless unit

        result = nil
        o.window(:group, unit, nil) { result = super }
        # [ADR 0114] An example a `before(:context)` failed never runs, and is told failed.
        o.guard("rspec group") do
          kept = ::RSpec.world.filtered_examples[self]
          kept.each do |ex|
            id = RSpecHooks.id_of(ex.metadata)
            next if o.seen?(unit, id) || ex.execution_result.status != :failed

            o.ended(unit, id, :failed)
          end
        end
        result
      end
    end
  end

  # [ADR 0114] Minitest 5 and 6, and Rails' runner: each test runs in a window, a class's run
  # around its tests is a shared setup, and left-out tests leave `runnable_methods`.
  module MinitestHooks
    module_function

    def install(o)
      HushrunRuby.activate(o, "minitest")
      o.note_version("minitest", ::Minitest::VERSION) if defined?(::Minitest::VERSION)
      ::Minitest::Test.singleton_class.prepend(ClassHooks)
      ::Minitest::Test.prepend(TestHooks)
      o.at_end { MinitestHooks.list(o) }
      if ::Minitest::Runnable.respond_to?(:run_suite)
        ::Minitest::Runnable.singleton_class.prepend(SuiteHooks6)
      else
        ::Minitest::Runnable.singleton_class.prepend(SuiteHooks5)
      end
    end

    def unit_of(klass, name)
      loc = klass.instance_method(name).source_location
      loc && HushrunRuby.observer.rel(loc[0])
    rescue NameError
      nil
    end

    def id_of(klass, name)
      klass.name ? "#{klass.name}##{name}" : nil
    end

    # Every test file of the loaded classes.
    def list(o)
      ::Minitest::Runnable.runnables.each do |klass|
        next unless klass <= ::Minitest::Test

        klass.public_instance_methods(true).grep(/\Atest_/).each do |name|
          unit = unit_of(klass, name)
          o.list_unit(unit) if unit
        end
      end
    end

    module ClassHooks
      def runnable_methods
        all = super
        o = HushrunRuby.obs
        return all unless o

        kept = all
        o.guard("minitest selection") do
          @hushrun_counted ||= {}
          kept = all.reject do |name|
            unit = MinitestHooks.unit_of(self, name)
            id = MinitestHooks.id_of(self, name)
            out = unit && id && o.leave_out?(unit, id)
            o.deselected(unit, id) if out && !@hushrun_counted.key?(name)
            out
          end
          kept.each do |name|
            next if @hushrun_counted.key?(name)

            unit = MinitestHooks.unit_of(self, name)
            next unless unit

            o.selected(unit, 1)
            o.test(unit, MinitestHooks.id_of(self, name)) if o.by_test?
          end
          all.each { |name| @hushrun_counted[name] = true }
        end
        kept
      end
    end

    module TestHooks
      def run
        o = HushrunRuby.obs
        unit = o && MinitestHooks.unit_of(self.class, name)
        return super unless unit

        id = MinitestHooks.id_of(self.class, name)
        result = o.ran_test(unit, id) { super }
        o.guard("minitest test") do
          status = if result.respond_to?(:skipped?) && result.skipped?
                     :skipped
                   elsif result.respond_to?(:passed?) && result.passed?
                     :passed
                   else
                     :failed
                   end
          o.ended(unit, id, status)
        end
        result
      end
    end

    module SuiteHooks5
      def run(reporter, options = {})
        o = HushrunRuby.obs
        return super unless o && self <= ::Minitest::Test

        # Rails' runner loads the test files as Minitest starts: they are loaded once a class runs.
        o.run_started
        unit = MinitestHooks.class_unit(self)
        return super unless unit

        result = nil
        o.window(:group, unit, nil) { result = super }
        result
      end
    end

    module SuiteHooks6
      def run_suite(reporter, options = {})
        o = HushrunRuby.obs
        return super unless o && self <= ::Minitest::Test

        o.run_started
        unit = MinitestHooks.class_unit(self)
        return super unless unit

        result = nil
        o.window(:group, unit, nil) { result = super }
        result
      end
    end

    # The test file of a class's tests, when they are all in one.
    def class_unit(klass)
      units = klass.public_instance_methods(true).grep(/\Atest_/).map { |n| unit_of(klass, n) }.uniq
      units.size == 1 ? units.first : nil
    end
  end
end

HushrunRuby.start
