<?php

declare(strict_types=1);

namespace Pxp;

/**
 * A content-addressed cache of transpiled PHP.
 *
 * The key is `hash(source bytes) + toolchain version`, so editing a file or
 * upgrading the binary rebuilds the entry and nothing else. We hand back a real
 * file path (never an eval'd string) so PHP's opcache can cache the *generated*
 * PHP — the transpile cost is paid once per change, not per request.
 */
final class Cache
{
    public function __construct(
        private string $dir,
        private Transpiler $transpiler,
    ) {}

    /** Path to the cached PHP for `$source`, transpiling on a miss. */
    public function php(string $source): string
    {
        $bytes = file_get_contents($source);
        if ($bytes === false) {
            throw new \RuntimeException("pxp: cannot read {$source}");
        }
        $key = hash('xxh128', $bytes) . '-' . $this->transpiler->version();
        $out = "{$this->dir}/{$key}.php";

        if (!is_file($out)) {
            $this->write($out, $this->transpiler->transpile($source));
        }
        return $out;
    }

    /** Atomic write (temp + rename) so a concurrent reader never sees a partial file. */
    private function write(string $path, string $contents): void
    {
        $dir = \dirname($path);
        if (!is_dir($dir)) {
            @mkdir($dir, 0o777, true);
        }
        $tmp = $path . '.' . getmypid() . '.tmp';
        file_put_contents($tmp, $contents);
        rename($tmp, $path);
    }
}
