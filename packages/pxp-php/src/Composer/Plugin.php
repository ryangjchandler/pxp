<?php

declare(strict_types=1);

namespace Pxp\Composer;

use Composer\Composer;
use Composer\IO\IOInterface;
use Composer\Plugin\Capability\CommandProvider as CommandProviderCapability;
use Composer\Plugin\Capable;
use Composer\Plugin\PluginInterface;

/**
 * The Composer-command side of pxp. The *runtime* autoloader is registered
 * separately by `src/bootstrap.php` (Composer `autoload.files`); this class only
 * contributes the `pxp:build` command for the production build step.
 */
final class Plugin implements PluginInterface, Capable
{
    public function activate(Composer $composer, IOInterface $io): void {}

    public function deactivate(Composer $composer, IOInterface $io): void {}

    public function uninstall(Composer $composer, IOInterface $io): void {}

    public function getCapabilities(): array
    {
        return [CommandProviderCapability::class => CommandProvider::class];
    }
}
