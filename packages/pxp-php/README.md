# pxp/pxp

The PHP runtime for [PXP](../../). Write `.pxp` files (a PHP superset); they
transpile-on-load in development (cached) and are prebuilt for production.

This package lives in the pxp monorepo under `packages/pxp-php/` and will be
git-subsplit into its own repo for Packagist publishing. Everything here is
runnable locally without publishing.

## How it works

- **Transpile-on-load** ([`Autoloader`](src/Autoloader.php)) — reuses Composer's
  PSR-4 map but resolves `.pxp`. Registered *before* Composer's loader, so `.php`
  and `.pxp` coexist and adoption is incremental.
- **Cache** ([`Cache`](src/Cache.php)) — keyed on `hash(source) + toolchain
  version`; hands back a real file so opcache caches the generated PHP. The `pxp`
  binary is only invoked on a miss.
- **Build** ([`Builder`](src/Builder.php) / `composer pxp:build`) — pre-transpiles
  everything into the cache so production runs binary-free.

See [`../../docs/adoption.md`](../../docs/adoption.md) for the full design.

## Test it locally (no Composer install needed)

From the repo root, build the transpiler once:

```bash
cargo build --release
export PXP_BIN="$PWD/target/release/pxp"
```

**Dev (transpile-on-load):**

```bash
php packages/pxp-php/example/run.php
# scale([1,2,3]) by 3 = 3, 6, 9
```

`example/src/Money.pxp` uses a multi-line short closure; it's transpiled on first
reference, cached under `example/.cache/`, and runs as ordinary PHP.

**Prod (prebuild):**

```bash
php packages/pxp-php/bin/pxp-build.php packages/pxp-php/example/.cache packages/pxp-php/example/src
```

Both produce the same content-hash cache entry — build once, serve from cache.

## The eventual app setup

```bash
composer require pxp/pxp
```

```jsonc
// app composer.json (optional — defaults to your PSR-4 app roots)
"extra": { "pxp": { "source": ["src/"], "cache": ".pxp/cache" } },
"scripts": { "post-autoload-dump": ["@php composer pxp:build"] }
```

Then write `.pxp` files with your normal namespaces. Dev transpiles on load; a
deploy `composer install --no-dev` triggers `pxp:build`, and production serves
cached PHP with no binary at request time.
