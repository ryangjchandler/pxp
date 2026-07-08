<?php

declare(strict_types=1);

namespace Pxp;

/**
 * Eagerly warms the cache for every `.pxp` file under the given roots. Used by
 * the `pxp:build` Composer command (prod) and testable standalone.
 */
final class Builder
{
    public function __construct(private Cache $cache) {}

    /**
     * @param string[] $dirs
     * @return int number of files transpiled/cached
     */
    public function warm(array $dirs): int
    {
        $count = 0;
        foreach ($dirs as $dir) {
            if (!is_dir($dir)) {
                continue;
            }
            $it = new \RecursiveIteratorIterator(
                new \RecursiveDirectoryIterator($dir, \FilesystemIterator::SKIP_DOTS),
            );
            foreach ($it as $file) {
                if ($file->isFile() && strtolower($file->getExtension()) === 'pxp') {
                    $this->cache->php($file->getPathname());
                    $count++;
                }
            }
        }
        return $count;
    }
}
