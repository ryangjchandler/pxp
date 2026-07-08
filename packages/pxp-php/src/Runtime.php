<?php

declare(strict_types=1);

namespace Pxp;

/**
 * Wires the pieces together and registers the autoloader. Two entry points:
 *
 * - {@see Runtime::register()} — explicit; used by tests and manual setups.
 * - {@see Runtime::boot()} — best-effort auto-config when installed via Composer
 *   (reads Composer's PSR-4 map, scoped to the app's own source roots).
 */
final class Runtime
{
    private static bool $booted = false;

    /** @param array<string, string[]> $psr4 */
    public static function register(array $psr4, string $cacheDir, Transpiler $transpiler): Autoloader
    {
        $loader = new Autoloader($psr4, new Cache($cacheDir, $transpiler));
        spl_autoload_register([$loader, 'loadClass'], true, /* prepend */ true);
        return $loader;
    }

    /** Auto-configure inside a Composer-managed app. Safe to call always. */
    public static function boot(): void
    {
        if (self::$booted) {
            return;
        }
        self::$booted = true;

        try {
            $composer = self::findComposerLoader();
            if ($composer === null) {
                return; // not a Composer context — caller should use register()
            }
            $psr4 = self::appPrefixes($composer->getPrefixesPsr4());
            if ($psr4 === []) {
                return;
            }
            self::register($psr4, self::defaultCacheDir(), Transpiler::detect());
        } catch (\Throwable) {
            // Never break application boot because of pxp.
        }
    }

    private static function findComposerLoader(): ?\Composer\Autoload\ClassLoader
    {
        foreach (spl_autoload_functions() as $fn) {
            if (\is_array($fn) && ($fn[0] ?? null) instanceof \Composer\Autoload\ClassLoader) {
                return $fn[0];
            }
        }
        return null;
    }

    /**
     * Keep only PSR-4 prefixes that point at the app's own code, not vendor — so
     * running-first doesn't cost a filesystem stat on every vendor class load.
     *
     * @param array<string, string[]> $prefixes
     * @return array<string, string[]>
     */
    private static function appPrefixes(array $prefixes): array
    {
        $app = [];
        foreach ($prefixes as $prefix => $dirs) {
            $dirs = array_values(array_filter($dirs, static fn($d) => !str_contains($d, '/vendor/')));
            if ($dirs !== []) {
                $app[$prefix] = $dirs;
            }
        }
        return $app;
    }

    private static function defaultCacheDir(): string
    {
        // vendor/pxp/pxp/src -> project root
        return \dirname(__DIR__, 4) . '/.pxp/cache';
    }
}
