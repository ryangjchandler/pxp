<?php

declare(strict_types=1);

namespace Pxp;

/**
 * The transpile-on-load autoloader. It reuses Composer's PSR-4 map (namespace
 * prefix -> directories), just resolving `.pxp` instead of `.php`.
 *
 * Registered with `prepend = true` so it runs *before* Composer's loader (which
 * sidesteps `--classmap-authoritative`). If it finds a `.pxp` it handles it; if
 * not it returns and Composer loads the `.php` as normal — so `.php` and `.pxp`
 * files coexist with natural precedence and adoption is incremental.
 */
final class Autoloader
{
    /** @param array<string, string[]> $psr4 prefix => dirs, longest-prefix-first */
    public function __construct(
        private array $psr4,
        private Cache $cache,
    ) {
        // PSR-4 resolves the most specific (longest) prefix first.
        uksort($this->psr4, static fn($a, $b) => \strlen($b) <=> \strlen($a));
    }

    public function loadClass(string $class): void
    {
        $file = $this->findPxpFile($class);
        if ($file !== null) {
            require $this->cache->php($file);
        }
    }

    private function findPxpFile(string $class): ?string
    {
        foreach ($this->psr4 as $prefix => $dirs) {
            if (!str_starts_with($class, $prefix)) {
                continue;
            }
            $relative = str_replace('\\', '/', substr($class, \strlen($prefix)));
            foreach ($dirs as $dir) {
                $candidate = "{$dir}/{$relative}.pxp";
                if (is_file($candidate)) {
                    return $candidate;
                }
            }
        }
        return null;
    }
}
