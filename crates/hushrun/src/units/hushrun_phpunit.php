<?php
// [ADR 0116] hushrun's PHPUnit and Pest adapter, second part: the bootstrap the prepend file gives
// the one PHPUnit or Pest that claimed the run. It registers a tracer of PHPUnit's events, starts
// following the files the process opens and the directories it lists, runs the bootstrap PHPUnit
// would have run, and when the run ends writes what it saw to `HUSHRUN_UNITS`, one file whole.
//
// What a test file read (C-30, C-31): every file the process had included or opened by the time
// its last test ended. PHP keeps a class once loaded and runs no code to find it again, so what a
// test uses that an earlier one loaded leaves no trace in it; everything loaded before is read
// instead, which can only read more. Leaving out a test file or a test removes it from the suite
// PHPUnit is about to run, after its own filters and before any of its hooks.
//
// In a process PHPUnit starts to run a test in isolation, it only runs that bootstrap.

declare(strict_types=1);

namespace Hushrun\PhpUnit;

const SCHEMA = 'hushrun.unit-facts/v1';
const STREAM_OPEN_FOR_INCLUDE = 0x00000080;

/** The `file` stream wrapper: every open, listing and look of the process goes through it. */
final class Files
{
    /** @var resource|null */
    public $context;
    /** @var resource|null */
    private $handle;
    /** @var resource|null */
    private $dir;
    private static bool $on = false;

    public static function start(): bool
    {
        if (!in_array('file', stream_get_wrappers(), true)) {
            return false;
        }
        if (!@stream_wrapper_unregister('file') || !@stream_wrapper_register('file', self::class)) {
            @stream_wrapper_restore('file');
            return false;
        }
        self::$on = true;
        return true;
    }

    public static function stop(): void
    {
        if (self::$on) {
            self::$on = false;
            @stream_wrapper_restore('file');
        }
    }

    /** Whether the wrapper is still the process's: another one replaced it when it isn't. */
    public static function holds(): bool
    {
        if (!self::$on) {
            return false;
        }
        $h = @fopen(__FILE__, 'rb');
        if ($h === false) {
            return false;
        }
        $meta = stream_get_meta_data($h);
        fclose($h);
        return ($meta['wrapper_data'] ?? null) instanceof self;
    }

    /** @template T @param callable():T $fn @return T */
    private static function native(callable $fn)
    {
        stream_wrapper_restore('file');
        try {
            return $fn();
        } finally {
            stream_wrapper_unregister('file');
            stream_wrapper_register('file', self::class);
        }
    }

    public function stream_open(string $path, string $mode, int $options, ?string &$opened): bool
    {
        if (($options & STREAM_OPEN_FOR_INCLUDE) === 0 && strpbrk($mode, 'r+') !== false) {
            State::opened($path);
        }
        $usePath = ($options & STREAM_USE_PATH) !== 0;
        $report = ($options & STREAM_REPORT_ERRORS) !== 0;
        $context = $this->context;
        $handle = self::native(static function () use ($path, $mode, $usePath, $report, $context) {
            if ($context !== null) {
                return $report ? fopen($path, $mode, $usePath, $context) : @fopen($path, $mode, $usePath, $context);
            }
            return $report ? fopen($path, $mode, $usePath) : @fopen($path, $mode, $usePath);
        });
        if ($handle === false) {
            return false;
        }
        $this->handle = $handle;
        if ($usePath) {
            $meta = stream_get_meta_data($handle);
            $opened = $meta['uri'] ?? $path;
        }
        return true;
    }

    public function stream_read(int $count)
    {
        return fread($this->handle, $count);
    }

    public function stream_write(string $data)
    {
        return fwrite($this->handle, $data);
    }

    public function stream_close(): void
    {
        if (is_resource($this->handle)) {
            fclose($this->handle);
        }
    }

    public function stream_eof(): bool
    {
        return feof($this->handle);
    }

    public function stream_tell()
    {
        return ftell($this->handle);
    }

    public function stream_seek(int $offset, int $whence): bool
    {
        return fseek($this->handle, $offset, $whence) === 0;
    }

    public function stream_flush(): bool
    {
        return fflush($this->handle);
    }

    public function stream_stat()
    {
        return fstat($this->handle);
    }

    public function stream_lock($operation): bool
    {
        if ($operation === 0 || $operation === null) {
            $operation = LOCK_EX;
        }
        return flock($this->handle, (int) $operation);
    }

    public function stream_truncate(int $size): bool
    {
        return ftruncate($this->handle, $size);
    }

    public function stream_set_option(int $option, int $arg1, ?int $arg2): bool
    {
        switch ($option) {
            case STREAM_OPTION_BLOCKING:
                return stream_set_blocking($this->handle, (bool) $arg1);
            case STREAM_OPTION_READ_TIMEOUT:
                return stream_set_timeout($this->handle, $arg1, (int) $arg2);
            case STREAM_OPTION_WRITE_BUFFER:
                return stream_set_write_buffer($this->handle, (int) $arg2) === 0;
            case STREAM_OPTION_READ_BUFFER:
                return stream_set_read_buffer($this->handle, (int) $arg2) === 0;
        }
        return false;
    }

    public function stream_cast(int $as)
    {
        return $this->handle ?? false;
    }

    public function stream_metadata(string $path, int $option, $value): bool
    {
        return self::native(static function () use ($path, $option, $value): bool {
            switch ($option) {
                case STREAM_META_TOUCH:
                    $value = (array) $value;
                    return isset($value[0]) ? touch($path, (int) $value[0], (int) ($value[1] ?? $value[0])) : touch($path);
                case STREAM_META_OWNER_NAME:
                case STREAM_META_OWNER:
                    return chown($path, $value);
                case STREAM_META_GROUP_NAME:
                case STREAM_META_GROUP:
                    return chgrp($path, $value);
                case STREAM_META_ACCESS:
                    return chmod($path, (int) $value);
            }
            return false;
        });
    }

    public function url_stat(string $path, int $flags)
    {
        return self::native(static function () use ($path, $flags) {
            $link = ($flags & STREAM_URL_STAT_LINK) !== 0;
            if (($flags & STREAM_URL_STAT_QUIET) !== 0) {
                return $link ? @lstat($path) : @stat($path);
            }
            return $link ? lstat($path) : stat($path);
        });
    }

    public function unlink(string $path): bool
    {
        $context = $this->context;
        return self::native(static fn (): bool => $context !== null ? unlink($path, $context) : unlink($path));
    }

    public function rename(string $from, string $to): bool
    {
        $context = $this->context;
        return self::native(static fn (): bool => $context !== null ? rename($from, $to, $context) : rename($from, $to));
    }

    public function mkdir(string $path, int $mode, int $options): bool
    {
        $recursive = ($options & STREAM_MKDIR_RECURSIVE) !== 0;
        $report = ($options & STREAM_REPORT_ERRORS) !== 0;
        $context = $this->context;
        return self::native(static function () use ($path, $mode, $recursive, $report, $context): bool {
            if ($context !== null) {
                return $report ? mkdir($path, $mode, $recursive, $context) : @mkdir($path, $mode, $recursive, $context);
            }
            return $report ? mkdir($path, $mode, $recursive) : @mkdir($path, $mode, $recursive);
        });
    }

    public function rmdir(string $path, int $options): bool
    {
        $context = $this->context;
        return self::native(static fn (): bool => $context !== null ? rmdir($path, $context) : rmdir($path));
    }

    public function dir_opendir(string $path, int $options): bool
    {
        $context = $this->context;
        $dir = self::native(static fn () => $context !== null ? @opendir($path, $context) : @opendir($path));
        if ($dir === false) {
            return false;
        }
        $this->dir = $dir;
        return true;
    }

    public function dir_readdir()
    {
        return readdir($this->dir);
    }

    public function dir_rewinddir(): bool
    {
        rewinddir($this->dir);
        return true;
    }

    public function dir_closedir(): bool
    {
        closedir($this->dir);
        return true;
    }
}

/** What one test is, and how it ended. */
final class TestSeen
{
    public string $unit;
    public string $id;
    public bool $started = false;
    public bool $finished = false;
    public bool $failed = false;
    public bool $skipped = false;
    public bool $deselected = false;
    public bool $neverLeftOut = false;
    public ?string $notKept = null;
    public float $since = 0.0;
    public int $durationMs = 0;
    /** @var list<array{string, string}> */
    public array $notFollowed = [];

    public function __construct(string $unit, string $id)
    {
        $this->unit = $unit;
        $this->id = $id;
    }
}

/** Everything the adapter knows of the run. */
final class State
{
    public static array $run = [];
    public static string $root = '';
    public static string $realRoot = '';
    public static string $out = '';
    public static bool $byTest = false;
    public static bool $followsFiles = false;
    /** @var array<string, int> each path the process opened, in order */
    public static array $opened = [];
    /** @var array<string, string> each class's unit */
    public static array $unitOf = [];
    /** @var list<string>|null the test files the suite holds, from the root */
    public static ?array $testFiles = null;
    /** @var array<string, TestSeen> by unit, "\0", id */
    public static array $tests = [];
    /** @var array<string, array{int, int}> how far the process had included and opened when each unit last ended */
    public static array $ends = [];
    /** @var array<string, int> how many of each unit's tests are selected to run */
    public static array $selected = [];
    /** @var array<string, true> */
    public static array $failedUnits = [];
    /** @var array<string, true> */
    public static array $deselectedUnits = [];
    /** @var array<string, list<array{string, string}>> */
    public static array $unitNotFollowed = [];
    /** @var list<array{string, string}> */
    public static array $notFollowed = [];
    public static bool $failuresByUnit = true;
    public static ?string $refused = null;
    public static ?string $leftOutUnlisted = null;
    public static ?string $notKept = null;
    /** @var list<string> */
    public static array $errors = [];
    public static ?TestSeen $current = null;
    public static bool $written = false;
    public static int $removed = 0;

    public static function opened(string $path): void
    {
        $path = self::absolute($path);
        if (!isset(self::$opened[$path])) {
            self::$opened[$path] = count(self::$opened);
        }
    }

    private static function absolute(string $path): string
    {
        if (strncmp($path, 'file://', 7) === 0) {
            $path = substr($path, 7);
        }
        if ($path === '' || $path[0] === '/' || preg_match('#^[A-Za-z]:[/\\\\]#', $path) === 1) {
            return $path;
        }
        return (getcwd() ?: '.') . '/' . $path;
    }

    /** `$path` from the root, `/`-separated; null when it isn't in it. */
    public static function fromRoot(string $path): ?string
    {
        $path = strtr($path, '\\', '/');
        $path = self::collapse($path);
        foreach ([self::$realRoot, self::$root] as $root) {
            if ($root !== '' && strncmp($path, $root . '/', strlen($root) + 1) === 0) {
                return substr($path, strlen($root) + 1);
            }
        }
        $real = @realpath($path);
        if (is_string($real)) {
            $real = strtr($real, '\\', '/');
            if (self::$realRoot !== '' && strncmp($real, self::$realRoot . '/', strlen(self::$realRoot) + 1) === 0) {
                return substr($real, strlen(self::$realRoot) + 1);
            }
        }
        return null;
    }

    private static function collapse(string $path): string
    {
        if (strpos($path, '/.') === false && strpos($path, '//') === false) {
            return $path;
        }
        $parts = [];
        foreach (explode('/', $path) as $i => $part) {
            if ($part === '..') {
                array_pop($parts);
            } elseif ($part !== '.' && ($part !== '' || $i === 0)) {
                $parts[] = $part;
            }
        }
        return implode('/', $parts);
    }

    /** The file a test class is declared in, from the root: Pest's for its own classes. */
    public static function unitOf(string $class): ?string
    {
        if (array_key_exists($class, self::$unitOf)) {
            return self::$unitOf[$class];
        }
        $file = null;
        try {
            $reflection = new \ReflectionClass($class);
            if ($reflection->hasProperty('__filename')) {
                $property = $reflection->getProperty('__filename');
                if ($property->isStatic()) {
                    $value = $property->getValue();
                    if (is_string($value)) {
                        $file = $value;
                    }
                }
            }
            if ($file === null) {
                $name = $reflection->getFileName();
                $file = is_string($name) ? $name : null;
            }
        } catch (\Throwable $e) {
            $file = null;
        }
        $unit = $file === null ? null : self::fromRoot($file);
        self::$unitOf[$class] = $unit;
        return $unit;
    }

    /** A test's unit and id as PHPUnit's events name it; null for one of no test file of the root. */
    public static function key($test): ?array
    {
        if ($test instanceof \PHPUnit\Event\Code\TestMethod) {
            $unit = self::unitOf($test->className());
            if ($unit === null) {
                return null;
            }
            $name = $test->methodName();
            $data = $test->testData();
            if ($data->hasDataFromDataProvider()) {
                $set = $data->dataFromDataProvider()->dataSetName();
                $name .= is_int($set) ? sprintf(' with data set #%d', $set) : sprintf(' with data set "%s"', $set);
            }
            return [$unit, self::id($test->className(), $name)];
        }
        if ($test instanceof \PHPUnit\Event\Code\Phpt) {
            $unit = self::fromRoot($test->file());
            return $unit === null ? null : [$unit, ''];
        }
        return null;
    }

    /** The unit and id of a test case object as the suite holds it. */
    public static function keyOf($test): ?array
    {
        if ($test instanceof \PHPUnit\Framework\TestCase) {
            $unit = self::unitOf(get_class($test));
            return $unit === null ? null : [$unit, self::id(get_class($test), $test->nameWithDataSet())];
        }
        if (class_exists(\PHPUnit\Runner\PhptTestCase::class, false) && $test instanceof \PHPUnit\Runner\PhptTestCase) {
            $file = \Closure::bind(fn () => $this->filename ?? null, $test, \PHPUnit\Runner\PhptTestCase::class)();
            $unit = is_string($file) ? self::fromRoot($file) : null;
            return $unit === null ? null : [$unit, ''];
        }
        return null;
    }

    /**
     * A test's id within its file: its class and its name with its data set, `Class::method with
     * data set #0`; for Pest, whose class is made from the file's path, its name alone, without
     * Pest's `__pest_evaluable_`.
     */
    public static function id(string $class, string $name): string
    {
        if (self::isPest($class)) {
            return strncmp($name, '__pest_evaluable_', 17) === 0 ? substr($name, 17) : $name;
        }
        return $class . '::' . $name;
    }

    /** @var array<string, bool> */
    private static array $pest = [];

    public static function isPest(string $class): bool
    {
        if (!isset(self::$pest[$class])) {
            $traits = class_exists($class, false) ? (class_uses($class) ?: []) : [];
            self::$pest[$class] = isset($traits['Pest\\Concerns\\Testable']);
        }
        return self::$pest[$class];
    }

    public static function test($event): ?TestSeen
    {
        if (!method_exists($event, 'test')) {
            return null;
        }
        $key = self::key($event->test());
        if ($key === null) {
            return null;
        }
        [$unit, $id] = $key;
        $name = $unit . "\0" . $id;
        if (!isset(self::$tests[$name])) {
            self::$tests[$name] = new TestSeen($unit, $id);
        }
        return self::$tests[$name];
    }

    public static function mark(string $unit): void
    {
        self::$ends[$unit] = [count(get_included_files()), count(self::$opened)];
    }
}

/** PHPUnit's events, every one, as they happen. */
final class Tracer implements \PHPUnit\Event\Tracer\Tracer
{
    public function trace(\PHPUnit\Event\Event $event): void
    {
        try {
            Run::on($event);
        } catch (\Throwable $e) {
            State::$errors[] = 'hushrun\'s PHPUnit adapter failed: ' . get_class($e) . ': ' . $e->getMessage();
        }
    }
}

final class Run
{
    /** The issues a test raised that fail the run, by event, with the configuration's switch. */
    private const FAILING = [
        'PHPUnit\Event\Test\PhpunitErrorTriggered' => null,
        'PHPUnit\Event\Test\PhpunitWarningTriggered' => null,
        'PHPUnit\Event\Test\ErrorTriggered' => null,
        'PHPUnit\Event\Test\WarningTriggered' => 'failOnWarning',
        'PHPUnit\Event\Test\PhpWarningTriggered' => 'failOnWarning',
        'PHPUnit\Event\Test\NoticeTriggered' => 'failOnNotice',
        'PHPUnit\Event\Test\PhpNoticeTriggered' => 'failOnNotice',
        'PHPUnit\Event\Test\DeprecationTriggered' => 'failOnDeprecation',
        'PHPUnit\Event\Test\PhpDeprecationTriggered' => 'failOnDeprecation',
        'PHPUnit\Event\Test\PhpunitDeprecationTriggered' => 'failOnPhpunitDeprecation',
        'PHPUnit\Event\Test\PhpunitNoticeTriggered' => 'failOnPhpunitNotice',
        'PHPUnit\Event\Test\ConsideredRisky' => 'failOnRisky',
    ];

    public static function on(\PHPUnit\Event\Event $event): void
    {
        $class = get_class($event);
        switch ($class) {
            case 'PHPUnit\Event\TestSuite\Loaded':
                self::loaded($event->testSuite());
                return;
            case 'PHPUnit\Event\TestRunner\ExecutionStarted':
                self::started();
                return;
            case 'PHPUnit\Event\Test\PreparationStarted':
                $test = State::test($event);
                if ($test !== null) {
                    $test->started = true;
                    $test->since = microtime(true);
                    State::$current = $test;
                    self::isolated($event->test(), $test);
                }
                return;
            case 'PHPUnit\Event\Test\Failed':
            case 'PHPUnit\Event\Test\Errored':
            case 'PHPUnit\Event\Test\PreparationFailed':
                $test = State::test($event);
                if ($test !== null) {
                    $test->failed = true;
                    State::$failedUnits[$test->unit] = true;
                } else {
                    State::$failuresByUnit = false;
                }
                return;
            case 'PHPUnit\Event\Test\Skipped':
            case 'PHPUnit\Event\Test\MarkedIncomplete':
                $test = State::test($event);
                if ($test !== null) {
                    $test->skipped = true;
                    $message = method_exists($event, 'message') ? (string) $event->message() : '';
                    if (strpos($message, 'depends on') !== false) {
                        // A test it depends on didn't run: it ran nothing a pass could stand on.
                        $test->notKept = 'a test it depends on didn\'t run, so it was skipped';
                    }
                    if (!$test->started) {
                        $test->finished = true;
                        State::mark($test->unit);
                    }
                }
                return;
            case 'PHPUnit\Event\Test\Finished':
                $test = State::test($event);
                if ($test !== null) {
                    $test->finished = $test->notKept === null;
                    $test->durationMs += (int) round((microtime(true) - ($test->since ?: microtime(true))) * 1000);
                    State::mark($test->unit);
                }
                State::$current = null;
                return;
            case 'PHPUnit\Event\Test\BeforeFirstTestMethodErrored':
            case 'PHPUnit\Event\Test\BeforeFirstTestMethodFailed':
            case 'PHPUnit\Event\Test\AfterLastTestMethodErrored':
            case 'PHPUnit\Event\Test\AfterLastTestMethodFailed':
                // A hook of its class failed: every test of the class failed with it.
                $unit = method_exists($event, 'testClassName') ? State::unitOf($event->testClassName()) : null;
                if ($unit === null) {
                    State::$failuresByUnit = false;
                    return;
                }
                State::$failedUnits[$unit] = true;
                $prefix = State::id($event->testClassName(), '');
                foreach (State::$tests as $test) {
                    if ($test->unit === $unit && ($prefix === '' || strncmp($test->id, $prefix, strlen($prefix)) === 0)) {
                        $test->failed = true;
                    }
                }
                State::mark($unit);
                return;
            case 'PHPUnit\Event\Application\Finished':
                Write::facts();
                return;
        }
        if (array_key_exists($class, self::FAILING)) {
            $switch = self::FAILING[$class];
            if ($switch !== null && !self::fails($switch)) {
                return;
            }
            $test = State::test($event);
            if ($test === null) {
                State::$failuresByUnit = false;
                return;
            }
            $test->failed = true;
            State::$failedUnits[$test->unit] = true;
            return;
        }
        // An issue no test owns (a warning while PHPUnit loaded the suite) may fail the run.
        if (strncmp($class, 'PHPUnit\Event\TestRunner\\', 25) === 0
            && (substr($class, -16) === 'WarningTriggered' || substr($class, -14) === 'ErrorTriggered'
                || substr($class, -20) === 'DeprecationTriggered' || substr($class, -15) === 'NoticeTriggered')) {
            State::$failuresByUnit = false;
        }
    }

    /** Whether the configuration fails the run on `$switch`; true when it can't be told. */
    private static function fails(string $switch): bool
    {
        try {
            $configuration = \PHPUnit\TextUI\Configuration\Registry::get();
        } catch (\Throwable $e) {
            return true;
        }
        if (!method_exists($configuration, $switch)) {
            return true;
        }
        return (bool) $configuration->{$switch}();
    }

    /** A test PHPUnit runs in a process of its own: what it read there isn't followed. */
    private static function isolated($code, TestSeen $test): void
    {
        $isolated = false;
        try {
            $configuration = \PHPUnit\TextUI\Configuration\Registry::get();
            $isolated = method_exists($configuration, 'processIsolation') && $configuration->processIsolation();
        } catch (\Throwable $e) {
        }
        if ($code instanceof \PHPUnit\Event\Code\Phpt) {
            $isolated = true;
        } elseif (!$isolated && $code instanceof \PHPUnit\Event\Code\TestMethod) {
            $metadata = $code->metadata();
            foreach (['isRunInSeparateProcess', 'isRunTestsInSeparateProcesses', 'isRunClassInSeparateProcess'] as $method) {
                if (method_exists($metadata, $method) && $metadata->{$method}()->isNotEmpty()) {
                    $isolated = true;
                }
            }
        }
        if ($isolated) {
            $test->notFollowed[] = ['isolated', 'it ran in a process of its own, whose files hushrun\'s adapter doesn\'t follow'];
        }
    }

    /** The test files the suite holds, whatever filters do with them. */
    private static function loaded($suite): void
    {
        $files = [];
        foreach ($suite->tests() as $test) {
            $key = State::key($test);
            if ($key !== null) {
                $files[$key[0]] = true;
            }
        }
        ksort($files, SORT_STRING);
        State::$testFiles = array_keys($files);
    }

    /** The suite PHPUnit is about to run, after its filters: what the plan lists is left out. */
    private static function started(): void
    {
        $suite = null;
        foreach (debug_backtrace(DEBUG_BACKTRACE_PROVIDE_OBJECT) as $frame) {
            if (($frame['function'] ?? '') === 'run'
                && ($frame['object'] ?? null) instanceof \PHPUnit\TextUI\TestRunner
                && ($frame['args'][2] ?? null) instanceof \PHPUnit\Framework\TestSuite) {
                $suite = $frame['args'][2];
                break;
            }
        }
        if ($suite === null) {
            State::$refused = 'hushrun\'s adapter didn\'t find the suite PHPUnit runs';
            State::$errors[] = 'hushrun\'s adapter didn\'t find the suite PHPUnit runs';
            return;
        }
        $reuse = array_fill_keys(array_map('strval', (array) (State::$run['reuse'] ?? [])), true);
        $reuseTests = [];
        if (State::$byTest) {
            foreach ((array) (State::$run['reuseTests'] ?? []) as $unit => $ids) {
                $reuseTests[(string) $unit] = array_fill_keys(array_map('strval', (array) $ids), true);
            }
        }
        $planned = State::$run['testFiles'] ?? null;
        if (($reuse !== [] || $reuseTests !== []) && is_array($planned)) {
            $listed = array_map('strval', $planned);
            sort($listed, SORT_STRING);
            if ($listed !== State::$testFiles) {
                State::$refused = 'PHPUnit\'s test files aren\'t the ones the plan listed';
                $reuse = [];
                $reuseTests = [];
            }
        }
        if (($reuse !== [] || $reuseTests !== []) && self::coverage()) {
            State::$refused = 'PHPUnit collects code coverage, and a partial run\'s coverage would read as a drop';
            $reuse = [];
            $reuseTests = [];
        }
        if (($reuse !== [] || $reuseTests !== []) && !State::$followsFiles) {
            State::$refused = 'the files the process opens can\'t be followed';
            $reuse = [];
            $reuseTests = [];
        }
        // What every test that stays depends on stays too.
        $cases = [];
        self::cases($suite, $cases);
        foreach ($cases as $case) {
            $key = State::keyOf($case);
            if ($key !== null && $case instanceof \PHPUnit\Framework\TestCase) {
                Related::add($key[0], get_class($case));
            }
        }
        $required = [];
        $hasDepends = [];
        foreach ($cases as $case) {
            if (method_exists($case, 'requires')) {
                foreach ($case->requires() as $dependency) {
                    $target = method_exists($dependency, 'getTarget') ? (string) $dependency->getTarget() : '';
                    if ($target !== '') {
                        $required[$target] = true;
                        $required[explode('::', $target)[0]] = true;
                        $hasDepends[get_class($case)] = true;
                    }
                }
            }
        }
        $keep = static function ($case) use ($required): bool {
            if (!$case instanceof \PHPUnit\Framework\TestCase) {
                return true;
            }
            return isset($required[get_class($case)]) || isset($required[get_class($case) . '::' . $case->name()]);
        };
        $remove = static function ($test) use ($reuse, $reuseTests, $keep, $hasDepends): bool {
            $key = State::keyOf($test);
            if ($key === null) {
                return false;
            }
            [$unit, $id] = $key;
            if (isset($reuse[$unit])) {
                return !$keep($test);
            }
            if ($id !== '' && isset($reuseTests[$unit][$id]) && !isset($hasDepends[get_class($test)])) {
                return !$keep($test);
            }
            return false;
        };
        if ($reuse !== [] || $reuseTests !== []) {
            self::prune($suite, $remove);
        }
        // What runs, after PHPUnit's filters and hushrun's.
        foreach (self::selected($suite) as $test) {
            $key = State::keyOf($test);
            if ($key === null) {
                continue;
            }
            [$unit, $id] = $key;
            State::$selected[$unit] = (State::$selected[$unit] ?? 0) + 1;
            $name = $unit . "\0" . $id;
            if (!isset(State::$tests[$name])) {
                State::$tests[$name] = new TestSeen($unit, $id);
            }
            if (isset($hasDepends[get_class($test)])) {
                State::$tests[$name]->neverLeftOut = true;
            }
        }
    }

    /** Whether PHPUnit collects code coverage in this run; true when it can't be told. */
    private static function coverage(): bool
    {
        if (!class_exists(\PHPUnit\Runner\CodeCoverage::class)) {
            return true;
        }
        try {
            return \PHPUnit\Runner\CodeCoverage::instance()->isActive();
        } catch (\Throwable $e) {
            return true;
        }
    }

    /** Every test case of `$suite`, whatever filters say. */
    private static function cases($suite, array &$out): void
    {
        foreach ($suite->tests() as $test) {
            if ($test instanceof \PHPUnit\Framework\TestSuite) {
                self::cases($test, $out);
            } else {
                $out[] = $test;
            }
        }
    }

    /** The tests of `$suite` that run, after its filters. */
    private static function selected($suite): array
    {
        $out = [];
        foreach ($suite as $test) {
            if ($test instanceof \PHPUnit\Framework\TestSuite) {
                foreach (self::selected($test) as $inner) {
                    $out[] = $inner;
                }
            } else {
                $out[] = $test;
            }
        }
        return $out;
    }

    /**
     * Removes the tests `$remove` names from `$suite`, recording each, and every suite left empty,
     * so that no hook of a class whose tests all go runs. Returns whether `$suite` is left empty.
     */
    private static function prune($suite, callable $remove): bool
    {
        $kept = [];
        $before = 0;
        foreach ($suite->tests() as $test) {
            $before++;
            if ($test instanceof \PHPUnit\Framework\TestSuite) {
                if (!self::prune($test, $remove)) {
                    $kept[] = $test;
                }
                continue;
            }
            if ($remove($test)) {
                [$unit, $id] = State::keyOf($test);
                $name = $unit . "\0" . $id;
                if (!isset(State::$tests[$name])) {
                    State::$tests[$name] = new TestSeen($unit, $id);
                }
                State::$tests[$name]->deselected = true;
                State::$deselectedUnits[$unit] = true;
                State::$removed++;
                continue;
            }
            $kept[] = $test;
        }
        if (count($kept) !== $before) {
            \Closure::bind(function () use ($kept): void {
                $this->tests = $kept;
                if (method_exists($this, 'clearCaches')) {
                    $this->clearCaches();
                }
            }, $suite, \PHPUnit\Framework\TestSuite::class)();
        }
        return $before > 0 && $kept === [];
    }
}

final class Write
{
    public static function facts(): void
    {
        if (State::$written) {
            return;
        }
        State::$written = true;
        $still = State::$followsFiles && Files::holds();
        Files::stop();
        if (State::$followsFiles && !$still) {
            State::$notFollowed[] = ['files', 'another stream wrapper replaced the one that follows the files the process opens'];
        }
        if (!State::$followsFiles) {
            State::$notFollowed[] = ['files', 'the files the process opens can\'t be followed'];
        }
        $included = get_included_files();
        $opened = array_keys(State::$opened);
        $rel = [];
        $from = static function (string $path) use (&$rel): ?string {
            if (!array_key_exists($path, $rel)) {
                $r = State::fromRoot($path);
                $rel[$path] = $r === null || Related::ownState($r) ? null : $r;
            }
            return $rel[$path];
        };
        $byUnit = [];
        foreach (State::$tests as $test) {
            $byUnit[$test->unit][] = $test;
        }
        foreach (State::$failedUnits as $unit => $_) {
            $byUnit[$unit] = $byUnit[$unit] ?? [];
        }
        // What the process read, once, in the order it read it: what each test file read is
        // all of it up to the end of its last test (`readTo`), so that the file grows with the
        // files read, not with them times the test files.
        $ends = [];
        foreach (array_keys($byUnit) as $unit) {
            $ends[$unit] = State::$ends[$unit] ?? [count($included), count($opened)];
        }
        uasort($ends, static fn ($a, $b) => [$a[0], $a[1]] <=> [$b[0], $b[1]]);
        $sequence = [];
        $inSequence = [];
        $readTo = [];
        $atIncluded = 0;
        $atOpened = 0;
        $add = static function (array $paths) use (&$sequence, &$inSequence, $from): void {
            foreach ($paths as $path) {
                $r = $from($path);
                if ($r !== null && !isset($inSequence[$r])) {
                    $inSequence[$r] = true;
                    $sequence[] = $r;
                }
            }
        };
        foreach ($ends as $unit => [$i, $o]) {
            if ($i > $atIncluded) {
                $add(array_slice($included, $atIncluded, $i - $atIncluded));
                $atIncluded = $i;
            }
            if ($o > $atOpened) {
                $add(array_slice($opened, $atOpened, $o - $atOpened));
                $atOpened = $o;
            }
            $readTo[$unit] = count($sequence);
        }
        // Another test file is read only where this one names one of its classes, or inherits
        // from it: PHPUnit includes every test file before the first test runs.
        $related = [];
        foreach (array_keys($byUnit) as $unit) {
            $of = Related::of($unit);
            if ($of !== []) {
                $related[$unit] = $of;
            }
        }
        $units = [];
        ksort($byUnit, SORT_STRING);
        foreach ($byUnit as $unit => $tests) {
            $deselected = isset(State::$deselectedUnits[$unit]) && array_filter($tests, static fn (TestSeen $t) => !$t->deselected) === [];
            $notFollowed = State::$unitNotFollowed[$unit] ?? [];
            $finished = 0;
            $skipped = 0;
            $failed = isset(State::$failedUnits[$unit]);
            $duration = 0;
            $out = [];
            foreach ($tests as $test) {
                if ($test->deselected) {
                    $out[] = ['id' => $test->id, 'deselected' => true];
                    continue;
                }
                $failed = $failed || $test->failed;
                if ($test->finished || $test->failed) {
                    $finished++;
                }
                if ($test->skipped && $test->finished) {
                    $skipped++;
                }
                $duration += $test->durationMs;
                foreach ($test->notFollowed as [$why, $detail]) {
                    $notFollowed[] = [$why, $detail];
                }
                $entry = [
                    'id' => $test->id,
                    'finished' => $test->finished || $test->failed,
                    'failed' => $test->failed,
                    'durationMs' => $test->durationMs,
                    'notFollowed' => array_map(static fn ($n) => ['why' => $n[0], 'detail' => $n[1]], $test->notFollowed),
                ];
                if ($test->skipped && !$test->failed) {
                    $entry['skipped'] = true;
                }
                if ($test->neverLeftOut || $test->id === '') {
                    $entry['neverLeftOut'] = true;
                }
                if ($test->notKept !== null && !$test->failed) {
                    $entry['finished'] = false;
                    $entry['notKept'] = $test->notKept;
                }
                $out[] = $entry;
            }
            $facts = [
                'unit' => $unit,
                'selected' => State::$selected[$unit] ?? 0,
                'finished' => $finished,
                'failed' => $failed,
                'deselected' => $deselected,
                'durationMs' => $duration,
                'reads' => [$unit],
                'skipped' => $skipped,
                'notFollowed' => array_map(static fn ($n) => ['why' => $n[0], 'detail' => $n[1]], $notFollowed),
            ];
            if (State::$byTest) {
                $facts['tests'] = $out;
            }
            $units[] = $facts;
        }
        usort($units, static fn ($a, $b) => strcmp($a['unit'], $b['unit']));
        $settings = [
            'sequence' => $sequence,
            'readTo' => (object) $readTo,
            'related' => (object) $related,
            'byName' => array_keys(Related::$classes),
            'runner' => State::$run['__runner'] ?? 'phpunit',
            'version' => class_exists(\PHPUnit\Runner\Version::class) ? \PHPUnit\Runner\Version::id() : null,
            'testFiles' => State::$testFiles ?? [],
            'removed' => State::$removed,
        ];
        if (State::$refused !== null) {
            $settings['refused'] = State::$refused;
        }
        $file = [
            'schema' => SCHEMA,
            'adapter' => 'phpunit',
            'failuresByUnit' => State::$failuresByUnit && State::$errors === [],
            'settings' => $settings,
            'notFollowed' => array_map(static fn ($n) => ['why' => $n[0], 'detail' => $n[1]], State::$notFollowed),
            'units' => $units,
            'errors' => State::$errors,
            'removedItems' => State::$removed,
        ];
        if (State::$testFiles === null) {
            $file['notKept'] = 'PHPUnit stopped before it loaded the test suite';
        }
        $json = json_encode($file, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_INVALID_UTF8_SUBSTITUTE);
        if ($json === false) {
            return;
        }
        $name = 'phpunit-' . getmypid() . '.json';
        $tmp = State::$out . '/.' . $name;
        if (@file_put_contents($tmp, $json) !== false) {
            @rename($tmp, State::$out . '/' . $name);
        }
    }
}

(static function (): void {
    $here = $GLOBALS['__hushrun_phpunit'] ?? null;
    if (!is_array($here)) {
        // A process PHPUnit started to run a test in isolation: the bootstrap it would have run.
        $user = getenv('HUSHRUN_PHPUNIT_BOOTSTRAP');
        if (is_string($user) && $user !== '') {
            define('HUSHRUN_PHPUNIT_USER_BOOTSTRAP', $user);
        }
        return;
    }
    unset($GLOBALS['__hushrun_phpunit']);
    $user = $here['bootstrap'];
    if ($user === null && !$here['noConfig']) {
        $user = Config::bootstrap($here['config'], $here['cwd']);
    }
    if ($user !== null) {
        define('HUSHRUN_PHPUNIT_USER_BOOTSTRAP', $user);
        putenv('HUSHRUN_PHPUNIT_BOOTSTRAP=' . $user);
    }
    $run = @file_get_contents($here['dir'] . '/hushrun.json');
    $run = is_string($run) ? json_decode($run, true) : null;
    if (!is_array($run)) {
        return;
    }
    $run['__runner'] = $here['runner'];
    State::$run = $run;
    State::$out = $here['dir'];
    State::$root = rtrim(strtr((string) ($run['root'] ?? ''), '\\', '/'), '/');
    $real = @realpath(State::$root);
    State::$realRoot = is_string($real) ? rtrim(strtr($real, '\\', '/'), '/') : State::$root;
    State::$byTest = ($run['unit'] ?? 'file') === 'test';
    if (!interface_exists(\PHPUnit\Event\Tracer\Tracer::class)) {
        return;
    }
    try {
        \PHPUnit\Event\Facade::instance()->registerTracer(new Tracer());
    } catch (\Throwable $e) {
        return;
    }
    State::$followsFiles = Files::start();
    register_shutdown_function(static function (): void {
        // A run PHP ended early (an `exit` in a test) still says what it saw.
        if (!State::$written) {
            State::$notKept = 'PHP exited before PHPUnit\'s run ended';
            State::$failuresByUnit = false;
            Write::facts();
        }
    });
})();

/** Which test files a test file's own code names. */
final class Related
{
    /** @var array<string, array<string, true>> each unit's names */
    private static array $names = [];
    /** @var array<string, array<string, true>> each unit's files its classes inherit from */
    private static array $ancestors = [];
    /** @var array<string, list<string>> each unit's classes */
    public static array $classes = [];

    public static function add(string $unit, string $class): void
    {
        if (!in_array($class, self::$classes[$unit] ?? [], true)) {
            self::$classes[$unit][] = $class;
        }
    }

    /** @var array<string, list<array{string, string}>>|null each class's short name, lowercase, to its full name and test file */
    private static ?array $index = null;

    /**
     * The other test files `$unit` reads: those its classes inherit from, and those one of whose
     * classes its own code, or its ancestors', names: by its short name, or by a qualified name
     * the class's full name ends with.
     *
     * @return list<string>
     */
    public static function of(string $unit): array
    {
        if (self::$index === null) {
            self::$index = [];
            foreach (self::$classes as $file => $classes) {
                foreach ($classes as $class) {
                    self::$index[strtolower(self::short($class))][] = [strtolower($class), $file];
                }
            }
        }
        $out = self::ancestors($unit);
        [$short, $qualified] = self::names($unit);
        foreach (array_keys(self::ancestors($unit)) as $file) {
            [$s, $q] = self::names($file);
            $short += $s;
            $qualified += $q;
        }
        // Its own classes' names are its own.
        foreach (self::$classes[$unit] ?? [] as $class) {
            unset($short[strtolower(self::short($class))]);
        }
        foreach (array_keys($short) as $name) {
            foreach (self::$index[$name] ?? [] as [, $file]) {
                $out[$file] = true;
            }
        }
        foreach (array_keys($qualified) as $name) {
            $name = ltrim($name, '\\');
            foreach (self::$index[strtolower(self::short($name))] ?? [] as [$full, $file]) {
                if ($full === $name || substr($full, -strlen($name) - 1) === '\\' . $name) {
                    $out[$file] = true;
                }
            }
        }
        unset($out[$unit]);
        $out = array_keys($out);
        sort($out, SORT_STRING);
        return $out;
    }

    private static function short(string $class): string
    {
        $at = strrpos($class, '\\');
        return $at === false ? $class : substr($class, $at + 1);
    }

    /** A file PHPUnit keeps for itself, which no test reads: its result cache. */
    public static function ownState(string $path): bool
    {
        $name = basename($path);
        return $name === '.phpunit.result.cache' || strpos('/' . $path, '/.phpunit.cache/') !== false;
    }

    /** @return array<string, true> the files of the root its classes' parents, traits and interfaces are in */
    private static function ancestors(string $unit): array
    {
        if (isset(self::$ancestors[$unit])) {
            return self::$ancestors[$unit];
        }
        $out = [];
        foreach (self::$classes[$unit] ?? [] as $class) {
            try {
                $reflection = new \ReflectionClass($class);
            } catch (\Throwable $e) {
                continue;
            }
            $related = array_merge(
                array_values(class_parents($class) ?: []),
                array_values(class_implements($class) ?: []),
                self::traits($reflection)
            );
            foreach ($related as $name) {
                try {
                    $file = (new \ReflectionClass($name))->getFileName();
                } catch (\Throwable $e) {
                    continue;
                }
                $r = is_string($file) ? State::fromRoot($file) : null;
                if ($r !== null && $r !== $unit) {
                    $out[$r] = true;
                }
            }
        }
        return self::$ancestors[$unit] = $out;
    }

    private static function traits(\ReflectionClass $class): array
    {
        $out = [];
        for ($c = $class; $c !== false; $c = $c->getParentClass()) {
            foreach ($c->getTraitNames() as $trait) {
                $out[] = $trait;
                try {
                    $out = array_merge($out, self::traits(new \ReflectionClass($trait)));
                } catch (\Throwable $e) {
                }
            }
        }
        return $out;
    }

    /**
     * Every name the file's code holds: bare names, and qualified ones whole, lowercase; a string
     * that looks like a class name is a qualified one.
     *
     * @return array{array<string, true>, array<string, true>}
     */
    private static function names(string $file): array
    {
        if (isset(self::$names[$file])) {
            return self::$names[$file];
        }
        $short = [];
        $qualified = [];
        $text = @file_get_contents(State::$realRoot . '/' . $file);
        if (is_string($text)) {
            foreach (@token_get_all($text) as $token) {
                if (!is_array($token)) {
                    continue;
                }
                if ($token[0] === T_STRING) {
                    $short[strtolower($token[1])] = true;
                } elseif ($token[0] === T_NAME_QUALIFIED || $token[0] === T_NAME_FULLY_QUALIFIED || $token[0] === T_NAME_RELATIVE) {
                    $name = strtolower($token[1]);
                    if (strncmp($name, 'namespace\\', 10) === 0) {
                        $name = substr($name, 10);
                    }
                    $qualified[$name] = true;
                } elseif ($token[0] === T_CONSTANT_ENCAPSED_STRING) {
                    $name = strtolower(trim(substr($token[1], 1, -1)));
                    $name = str_replace('\\\\', '\\', $name);
                    if (preg_match('/^\\\\?[a-z_][a-z0-9_]*(\\\\[a-z_][a-z0-9_]*)*$/', $name) === 1) {
                        if (strpos($name, '\\') === false) {
                            $short[$name] = true;
                        } else {
                            $qualified[$name] = true;
                        }
                    }
                }
            }
        }
        return self::$names[$file] = [$short, $qualified];
    }
}

/** The bootstrap a PHPUnit configuration names. */
final class Config
{
    public static function bootstrap(?string $config, string $cwd): ?string
    {
        $file = null;
        if ($config !== null) {
            if (is_dir($config)) {
                $file = self::find($config);
            } elseif (is_file($config)) {
                $file = $config;
            }
        } else {
            $file = self::find($cwd);
        }
        if ($file === null) {
            return null;
        }
        $doc = new \DOMDocument();
        if (!@$doc->load($file, LIBXML_NONET) || $doc->documentElement === null) {
            return null;
        }
        $bootstrap = trim($doc->documentElement->getAttribute('bootstrap'));
        if ($bootstrap === '') {
            return null;
        }
        if ($bootstrap[0] === '/' || preg_match('#^[A-Za-z]:[/\\\\]#', $bootstrap) === 1) {
            return $bootstrap;
        }
        return dirname($file) . DIRECTORY_SEPARATOR . $bootstrap;
    }

    private static function find(string $dir): ?string
    {
        foreach (['phpunit.xml', 'phpunit.dist.xml', 'phpunit.xml.dist'] as $name) {
            $path = rtrim($dir, '/\\') . DIRECTORY_SEPARATOR . $name;
            if (is_file($path)) {
                return realpath($path) ?: $path;
            }
        }
        return null;
    }
}

if (defined('HUSHRUN_PHPUNIT_USER_BOOTSTRAP')) {
    include_once HUSHRUN_PHPUNIT_USER_BOOTSTRAP;
}
