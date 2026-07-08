<?php

declare(strict_types=1);

// Standalone build (prod path), testable without Composer:
//   php packages/pxp-php/bin/pxp-build.php <cache-dir> <src-dir> [<src-dir>...]

require_once __DIR__ . '/../src/Transpiler.php';
require_once __DIR__ . '/../src/Cache.php';
require_once __DIR__ . '/../src/Builder.php';

$args = array_slice($argv, 1);
if (count($args) < 2) {
    fwrite(STDERR, "usage: pxp-build.php <cache-dir> <src-dir> [<src-dir>...]\n");
    exit(2);
}

$cacheDir = array_shift($args);
$transpiler = Pxp\Transpiler::detect();
$builder = new Pxp\Builder(new Pxp\Cache($cacheDir, $transpiler));

$started = microtime(true);
$count = $builder->warm($args);
printf("pxp build: %d file(s) into %s in %.1fms\n", $count, $cacheDir, (microtime(true) - $started) * 1000);
