<?php

declare(strict_types=1);

namespace Pxp;

/**
 * Invokes the `pxp` Rust binary to transpile a source file. Shelling out is only
 * ever hit on a cache miss (see {@see Cache}), so in steady state its cost is
 * amortized to ~zero.
 */
final class Transpiler
{
    public function __construct(private string $binary) {}

    /** Locate the binary: explicit env override, then vendor/bin, then PATH. */
    public static function detect(): self
    {
        $binary = getenv('PXP_BIN') ?: 'pxp';
        return new self($binary);
    }

    public function binary(): string
    {
        return $this->binary;
    }

    /**
     * A cache-busting fingerprint of the toolchain: the binary's mtime + size, so
     * rebuilding/upgrading the transpiler invalidates every cached file.
     */
    public function version(): string
    {
        $stat = @stat($this->binary);
        return $stat ? "{$stat['mtime']}-{$stat['size']}" : 'unknown';
    }

    /** Transpile a source file, returning the generated PHP bytes. */
    public function transpile(string $sourceFile): string
    {
        $descriptors = [1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
        $proc = @proc_open([$this->binary, 'transpile', $sourceFile], $descriptors, $pipes);
        if (!\is_resource($proc)) {
            throw new \RuntimeException("pxp: cannot run binary '{$this->binary}'");
        }
        $out = stream_get_contents($pipes[1]);
        $err = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);
        $code = proc_close($proc);
        if ($code !== 0) {
            throw new \RuntimeException("pxp: transpile of {$sourceFile} failed ({$code}): {$err}");
        }
        return $out;
    }
}
