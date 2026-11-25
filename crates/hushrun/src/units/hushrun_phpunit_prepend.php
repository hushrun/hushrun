<?php
// [ADR 0116] hushrun's PHPUnit and Pest adapter, first part: every PHP process of COMMAND runs
// this file first (`auto_prepend_file`, from the ini file hushrun names in `PHP_INI_SCAN_DIR`).
// It does nothing unless the process is PHPUnit's or Pest's own program and the run's directory
// (`HUSHRUN_UNITS`) is unclaimed: then it claims the directory and gives the program
// `--bootstrap <hushrun's bootstrap>`, which loads the adapter and then the bootstrap the program
// would have loaded. It reads no file of the repository and writes nothing else.

(static function (): void {
    // The prepend file the PHP configuration named before hushrun's replaced it runs first, as
    // it would have.
    $before = getenv('HUSHRUN_PHP_PREPEND');
    if (is_string($before) && $before !== '' && is_file($before)) {
        require_once $before;
    }
    if (PHP_SAPI !== 'cli' || defined('HUSHRUN_PHPUNIT_BOOTSTRAP')) {
        return;
    }
    $dir = getenv('HUSHRUN_UNITS');
    $argv = $_SERVER['argv'] ?? null;
    if (!is_string($dir) || $dir === '' || !is_array($argv) || !isset($argv[0]) || !is_string($argv[0])) {
        return;
    }
    $program = strtr($argv[0], '\\', '/');
    $name = strtolower(basename($program));
    $name = preg_replace('/\.(bat|cmd|exe|phar)$/', '', $name);
    if ($name === 'phpunit') {
        $runner = 'phpunit';
    } elseif ($name === 'pest') {
        $runner = 'pest';
    } else {
        return;
    }
    // One process claims the run: the first PHPUnit or Pest to start. A PHPUnit a test starts,
    // or any after, finds the claim and stays out.
    $claim = @fopen($dir . '/.php-claim', 'x');
    if ($claim === false) {
        return;
    }
    fwrite($claim, (string) getmypid());
    fclose($claim);

    $bootstrap = null;
    $config = null;
    $noConfig = false;
    $out = [$argv[0]];
    $count = count($argv);
    for ($i = 1; $i < $count; $i++) {
        $word = (string) $argv[$i];
        if ($word === '--') {
            for (; $i < $count; $i++) {
                $out[] = $argv[$i];
            }
            break;
        }
        if ($word === '--bootstrap' && $i + 1 < $count) {
            $bootstrap = (string) $argv[++$i];
            continue;
        }
        if (strncmp($word, '--bootstrap=', 12) === 0) {
            $bootstrap = substr($word, 12);
            continue;
        }
        if (($word === '-c' || $word === '--configuration') && $i + 1 < $count) {
            $config = (string) $argv[$i + 1];
        } elseif (strncmp($word, '--configuration=', 16) === 0) {
            $config = substr($word, 16);
        } elseif ($word === '--no-configuration') {
            $noConfig = true;
        }
        $out[] = $word;
    }
    $cwd = getcwd() ?: '.';
    $absolute = static function (string $path, string $base): string {
        if ($path === '' || $path[0] === '/' || $path[0] === '\\' || preg_match('#^[A-Za-z]:[/\\\\]#', $path)) {
            return $path;
        }
        return rtrim($base, '/\\') . DIRECTORY_SEPARATOR . $path;
    };
    $GLOBALS['__hushrun_phpunit'] = [
        'runner' => $runner,
        'dir' => $dir,
        // The bootstrap COMMAND names, from where it runs; else the configuration's, which the
        // adapter reads once PHPUnit has found it.
        'bootstrap' => $bootstrap === null ? null : $absolute($bootstrap, $cwd),
        'config' => $config === null ? null : $absolute($config, $cwd),
        'noConfig' => $noConfig,
        'cwd' => $cwd,
        'argv' => $argv,
    ];
    $ours = __DIR__ . DIRECTORY_SEPARATOR . 'hushrun_phpunit.php';
    array_splice($out, 1, 0, ['--bootstrap', $ours]);
    $_SERVER['argv'] = $out;
    $_SERVER['argc'] = count($out);
    $GLOBALS['argv'] = $out;
    $GLOBALS['argc'] = count($out);
})();
