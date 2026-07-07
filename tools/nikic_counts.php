<?php
// Reference structural-count dumper using nikic/PHP-Parser.
// Usage: php nikic_counts.php <autoload.php> <listfile>   (listfile: one path per line)
// Output per file: "<path>\t<f>,<class>,<iface>,<trait>,<enum>,<method>,<prop>,<const>,<case>,<closure>,<arrow>"

require $argv[1];

use PhpParser\ParserFactory;
use PhpParser\NodeTraverser;
use PhpParser\NodeVisitorAbstract;
use PhpParser\Node;

$parser = method_exists(ParserFactory::class, 'createForNewestSupportedVersion')
    ? (new ParserFactory())->createForNewestSupportedVersion()
    : (new ParserFactory())->create(ParserFactory::PREFER_PHP7);

class Counter extends NodeVisitorAbstract {
    public array $c = [];
    private function add(string $k, int $v = 1): void { $this->c[$k] = ($this->c[$k] ?? 0) + $v; }
    public function enterNode(Node $n) {
        if ($n instanceof Node\Stmt\Function_) $this->add('func');
        elseif ($n instanceof Node\Stmt\Interface_) $this->add('iface');
        elseif ($n instanceof Node\Stmt\Trait_) $this->add('trait');
        elseif ($n instanceof Node\Stmt\Enum_) $this->add('enum');
        elseif ($n instanceof Node\Stmt\Class_) { if ($n->name !== null) $this->add('class'); }
        elseif ($n instanceof Node\Stmt\ClassMethod) $this->add('method');
        elseif ($n instanceof Node\Stmt\Property) $this->add('prop', count($n->props));
        elseif ($n instanceof Node\Stmt\ClassConst) $this->add('const', count($n->consts));
        elseif ($n instanceof Node\Stmt\EnumCase) $this->add('case');
        elseif ($n instanceof Node\Expr\Closure) $this->add('closure');
        elseif ($n instanceof Node\Expr\ArrowFunction) $this->add('arrow');
    }
}

$keys = ['func', 'class', 'iface', 'trait', 'enum', 'method', 'prop', 'const', 'case', 'closure', 'arrow'];

$fh = fopen($argv[2], 'r');
while (($line = fgets($fh)) !== false) {
    $path = trim($line);
    if ($path === '') continue;
    $code = @file_get_contents($path);
    if ($code === false) { echo "$path\tERR\n"; continue; }
    try {
        $ast = $parser->parse($code);
    } catch (\Throwable $e) {
        echo "$path\tERR\n";
        continue;
    }
    if ($ast === null) { echo "$path\tERR\n"; continue; }
    $counter = new Counter();
    $trav = new NodeTraverser();
    $trav->addVisitor($counter);
    $trav->traverse($ast);
    $row = array_map(fn($k) => $counter->c[$k] ?? 0, $keys);
    echo $path . "\t" . implode(',', $row) . "\n";
}
