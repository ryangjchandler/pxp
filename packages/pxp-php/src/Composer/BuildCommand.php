<?php

declare(strict_types=1);

namespace Pxp\Composer;

use Composer\Command\BaseCommand;
use Composer\Factory;
use Pxp\Builder;
use Pxp\Cache;
use Pxp\Transpiler;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

/**
 * `composer pxp:build` — pre-transpiles every `.pxp` source into the cache so
 * production serves cached PHP with no binary at request time. Wire this to
 * `post-autoload-dump` in the app's composer.json to build on deploy.
 */
final class BuildCommand extends BaseCommand
{
    protected function configure(): void
    {
        $this->setName('pxp:build')
            ->setDescription('Pre-transpile all .pxp sources into the pxp cache.');
    }

    protected function execute(InputInterface $input, OutputInterface $output): int
    {
        $composer = $this->requireComposer();
        $root = \dirname(Factory::getComposerFile());
        $extra = $composer->getPackage()->getExtra()['pxp'] ?? [];

        // Sources: explicit `extra.pxp.source`, else the app's own PSR-4 roots.
        $dirs = [];
        foreach ((array) ($extra['source'] ?? []) as $s) {
            $dirs[] = $root . '/' . ltrim((string) $s, '/');
        }
        if ($dirs === []) {
            foreach ($composer->getPackage()->getAutoload()['psr-4'] ?? [] as $paths) {
                foreach ((array) $paths as $p) {
                    $dirs[] = $root . '/' . ltrim((string) $p, '/');
                }
            }
        }

        $cacheDir = $root . '/' . ($extra['cache'] ?? '.pxp/cache');
        $count = (new Builder(new Cache($cacheDir, Transpiler::detect())))->warm($dirs);

        $output->writeln("<info>pxp:</info> transpiled {$count} file(s) into {$cacheDir}");
        return 0;
    }
}
