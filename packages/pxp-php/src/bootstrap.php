<?php

declare(strict_types=1);

// Loaded on every request via Composer's `autoload.files`. Self-contained
// (require_once the runtime classes) so it also works standalone, without
// Composer autoloading pxp's own classes yet.

require_once __DIR__ . '/Transpiler.php';
require_once __DIR__ . '/Cache.php';
require_once __DIR__ . '/Autoloader.php';
require_once __DIR__ . '/Runtime.php';

Pxp\Runtime::boot();
