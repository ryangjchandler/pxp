<?php

declare(strict_types=1);

// Standalone demo of transpile-on-load — no Composer install needed.
//   $ cargo build --release            # from the repo root, builds the `pxp` binary
//   $ php packages/pxp-php/example/run.php

require __DIR__ . '/../src/bootstrap.php';

// In a real app this comes from Composer's PSR-4 map (Runtime::boot()); here we
// register explicitly and point at the freshly-built binary.
$binary = getenv('PXP_BIN') ?: \dirname(__DIR__, 3) . '/target/release/pxp';

Pxp\Runtime::register(
    ['App\\' => [__DIR__ . '/src']],   // App\Foo -> example/src/Foo.pxp
    __DIR__ . '/.cache',
    new Pxp\Transpiler($binary),
);

// `App\Money` lives in Money.pxp — it transpiles on first reference, is cached,
// and runs as ordinary PHP.
$money = new App\Money(3);

echo "scale([1,2,3]) by 3 = ", implode(', ', $money->scale([1, 2, 3])), "\n";
