# Adoption: the `pxp/pxp` runtime package

The Rust binary in this repo is the transpiler. This document designs the **PHP
side** — the Composer package a user installs to actually *use* PXP in an app.

## The DX target

```bash
composer require pxp/pxp
```

…then write `src/Foo.pxp` with your normal namespace and it Just Works — no build
step in dev, source-accurate stack traces, `.php` files and `.pxp` files living
side by side. Production does one pre-transpile and never runs the binary at
request time.

This is TypeScript-grade DX: `.pxp` source, transpile-on-load with a cache, an
explicit build for prod.

---

## Package shape

One Composer package plays **two roles**:

| Role | When it runs | Provided by |
|---|---|---|
| Runtime bootstrap | every app request | `autoload.files` → `bootstrap.php` |
| Composer plugin | `composer` commands | `type: composer-plugin` → `PxpPlugin` |

```jsonc
// pxp/pxp — composer.json
{
  "name": "pxp/pxp",
  "type": "composer-plugin",
  "require": { "php": ">=8.1" },
  "autoload": {
    "psr-4": { "Pxp\\": "src/" },
    "files": ["src/bootstrap.php"]        // registers the autoloader every request
  },
  "extra": { "class": "Pxp\\PxpPlugin" }, // the composer-command side
  "bin": []                               // platform binary resolved at runtime (see below)
}
```

`bootstrap.php` is included on every request (Composer's `files` autoload) and
registers the on-load autoloader + the exception-trace translator.

---

## Transpile-on-load autoloader (+ PSR-4 interop)

The key idea: **reuse Composer's own PSR-4 map**, just swap `.php` → `.pxp`. No
separate config, and `.php`/`.pxp` files coexist with natural precedence.

```php
final class Autoloader
{
    /** @param array<string,string[]> $psr4  prefix => [dirs] (from Composer) */
    public function __construct(
        private array $psr4,
        private Cache $cache,
        private Transpiler $transpiler,
    ) {}

    public function loadClass(string $class): bool
    {
        $file = $this->findPxpFile($class);
        if ($file === null) {
            return false; // not a .pxp class — let Composer handle the .php
        }
        // Transpile (cached) to a real file so opcache can cache the *output*.
        $generated = $this->cache->php($file, $this->transpiler);
        require $generated;
        return true;
    }

    private function findPxpFile(string $class): ?string
    {
        $logical = str_replace('\\', '/', $class);
        foreach ($this->psr4 as $prefix => $dirs) {
            if (!str_starts_with($class, $prefix)) {
                continue;
            }
            $rest = substr($logical, strlen(str_replace('\\', '/', $prefix)));
            foreach ($dirs as $dir) {
                $candidate = "$dir/$rest.pxp";
                if (is_file($candidate)) {
                    return $candidate;
                }
            }
        }
        return null;
    }
}
```

Registration (in `bootstrap.php`):

```php
$composer = pxp_find_composer_loader();          // scan spl_autoload_functions()
$loader   = new Pxp\Autoloader(
    pxp_pxp_source_prefixes($composer),          // Composer PSR-4, scoped to app roots
    new Pxp\Cache(pxp_cache_dir()),
    Pxp\Transpiler::detect(),
);
spl_autoload_register([$loader, 'loadClass'], true, /* prepend */ true);
```

**Two decisions baked in here:**

1. **`prepend = true`.** We run *before* Composer's autoloader. This sidesteps
   `--classmap-authoritative` (which would otherwise say "unknown class → doesn't
   exist" and never fall through to us). If we find a `.pxp` we handle it; if not
   we return `false` and Composer loads the `.php` as normal.

2. **Scope to app source roots, not vendor.** Running first means our
   `findPxpFile` is consulted for *every* class load. To avoid a filesystem
   `stat` per vendor class, we only keep PSR-4 prefixes that point inside the
   project (drop anything under `vendor/`). Configurable via `extra.pxp.source`.

Result: `App\Foo` → Composer PSR-4 says `src/Foo.*`; we check `src/Foo.pxp`
first, else Composer serves `src/Foo.php`. Incremental adoption is free.

---

## Cache strategy

```php
final class Cache
{
    public function __construct(private string $dir) {}

    public function php(string $source, Transpiler $t): string
    {
        $bytes = file_get_contents($source);
        // Content hash + transpiler version + format version → any change rebuilds.
        $key   = hash('xxh128', $bytes) . '-' . $t->version() . '-1';
        $out   = "$this->dir/$key.php";
        if (!is_file($out)) {
            [$php, $map] = $t->transpile($bytes, $source); // bytes in, bytes + map out
            $this->atomicWrite($out, $php);
            $this->atomicWrite("$out.map", $map);           // sidecar for stack traces
        }
        return $out;
    }
}
```

- **Key = content hash of the source bytes + transpiler version.** Editing the
  file changes the hash → new cache entry. Upgrading the `pxp` binary changes the
  version → everything rebuilds. No mtime races, no stale output.
- **Location:** project-local `.pxp/cache/` (gitignored), configurable. Local so
  it's per-project and inspectable; the source map sidecars live beside the `.php`.
- **We `require` a real cache file, never `eval` a string** — so PHP's opcache
  caches the *generated* PHP and the transpile cost is paid once per change, not
  per request. The binary is only invoked on a cache miss.
- Cache filename is a hash (not the source path) — that's fine, because the
  exception translator (below) rewrites displayed paths back to the `.pxp` source
  via the `.map`, so users never see `.pxp/cache/ab12…php`.

---

## Invoking the transpiler + binary distribution

`Transpiler::transpile()` shells out to the `pxp` binary
(`pxp transpile <file>` → stdout, `.map` alongside). Process spawn is ~1ms and
**only happens on a cache miss**, so in steady state it's effectively free.

Distribution (the "no Rust toolchain required" requirement):

- **Bundle prebuilt binaries** per platform (linux x64/arm64, macOS x64/arm64,
  Windows), à la esbuild/tailwind. A `post-install-cmd` script picks/downloads
  the right one into `vendor/bin/`. `Transpiler::detect()` resolves it by
  `PHP_OS_FAMILY` + `php_uname('m')`.
- Later: a native extension via FFI to skip the spawn entirely — an optimization,
  not needed for v1 (the cache already amortizes spawn to near-zero).

---

## Production: build once, no binary at request time

Dev is lazy (transpile-on-load). Prod should be eager and binary-free:

```php
// PxpPlugin registers a `pxp:build` command AND hooks post-autoload-dump.
public function build(): void
{
    foreach ($this->allPxpSources() as $file) {
        $this->cache->php($file, $this->transpiler); // warm every entry
    }
}
```

- Wire `build()` to `post-autoload-dump`, so `composer install --no-dev` on
  deploy pre-transpiles the whole tree into `.pxp/cache/`.
- In prod the autoloader runs in **cache-only mode**: it `require`s the cached
  `.php` and *never* invokes the binary (errors loudly if an entry is missing).
  So **production servers don't need the `pxp` binary at all** — only the build
  machine does. This also means prod pays zero transpile cost and full opcache.

---

## Source maps → source-accurate stack traces (the payoff)

The transpiler already emits `.map` sidecars. This is where they earn their keep.

An uncaught exception's trace points at cache files (`…/ab12.php:42`). The
bootstrap registers a translator that rewrites each frame to the `.pxp` source:

```php
function pxp_translate_trace(array $frames): array {
    foreach ($frames as &$f) {
        if (isset($f['file']) && str_starts_with($f['file'], PXP_CACHE)) {
            [$src, $line] = Pxp\SourceMap::for($f['file'])->resolve($f['line']);
            $f['file'] = $src;   // src/Foo.pxp
            $f['line'] = $line;  // original line
        }
    }
    return $frames;
}
```

Honest caveat: a `Throwable`'s own `file`/`line` are immutable, so we rewrite at
the **display layer** (CLI/log formatter now; Whoops / Laravel Ignition / Symfony
ErrorHandler adapters next), not the exception object. Because most desugarings
are line-preserving, the map is usually identity for lines and only the *path*
needs fixing — cheap and correct.

(Alternative considered: a `pxp://` stream wrapper so PHP reports the `.pxp` path
directly. Rejected for v1 — stream-wrapper includes bypass opcache.)

---

## The setup walkthrough

```bash
composer require pxp/pxp            # installs runtime + platform binary
# add to composer.json: "extra": { "pxp": { "source": ["src/"] } }   (optional)
```

```php
// src/Money.pxp   (namespace App;)
final class Money {
    public function add(Money ...$others): static { /* … */ }
}
```

```php
// public/index.php — unchanged; `new App\Money()` transpiles on first load, cached.
```

Deploy: `composer install --no-dev` → `post-autoload-dump` pre-builds → prod runs
cached PHP with no binary.

---

## Decisions to confirm

1. **Cache location** — project `.pxp/cache/` (gitignored) vs `vendor/pxp/cache/`
   vs framework storage dir. Leaning project-local.
2. **Binary distribution** — bundle-per-platform vs download-on-install. Leaning
   download-on-install (smaller package, esbuild-style).
3. **Config source of truth** — reuse Composer PSR-4 automatically, or require an
   explicit `extra.pxp.source` allowlist. Leaning "auto, scoped to app roots,
   overridable."
4. **Prod safety** — cache-only autoloader that refuses to transpile at request
   time (fail if unbuilt), so a missing build is loud, not a silent binary
   dependency in prod. Leaning yes.
