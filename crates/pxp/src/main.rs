#[cfg(feature = "memprof")]
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
#[cfg(feature = "memprof")]
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use pxp::parser::parse_stats;
use pxp::sourcemap::SourceMap;
use pxp::{transpile, transpile_with_map};

const SOURCE_EXTS: &[&str] = &["php", "pxp"];

// A counting allocator so `mem-stats` can report real allocation behavior.
// Gated behind `memprof` so the production path uses the system allocator untaxed.
#[cfg(feature = "memprof")]
mod memprof {
    use super::*;

    pub static ALLOCS: AtomicUsize = AtomicUsize::new(0);
    pub static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc(layout);
            if !p.is_null() {
                ALLOCS.fetch_add(1, Relaxed);
                ALLOC_BYTES.fetch_add(layout.size(), Relaxed);
                let live = LIVE_BYTES.fetch_add(layout.size(), Relaxed) + layout.size();
                PEAK_BYTES.fetch_max(live, Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
            LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        Some("transpile") => cmd_transpile(&args[2..]),
        Some("build") => cmd_build(&args[2..]),
        Some("trace") => cmd_trace(&args[2..]),
        Some("parse-check") => cmd_parse_check(&args[2..]),
        Some("parse-debug") => cmd_parse_debug(&args[2..]),
        Some("fuzz") => cmd_fuzz(&args[2..]),
        Some("diff-nikic") => cmd_diff_nikic(&args[2..]),
        Some("mem-stats") => cmd_mem_stats(&args[2..]),
        _ => {
            usage();
            2
        }
    };
    std::process::exit(code);
}

fn usage() {
    eprintln!(
        "pxp — PHP superset transpiler\n\n\
         USAGE:\n\
         \x20\x20pxp transpile <file>            Transpile one file to stdout\n\
         \x20\x20pxp build <src_dir> <out_dir>   Transpile a tree (content-hash cached, writes .map sidecars)\n\
         \x20\x20pxp trace <generated.php> <line>  Translate a generated line back to pxp source\n"
    );
}

fn cmd_transpile(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("error: `transpile` needs a file path");
        return 2;
    };
    match std::fs::read(path) {
        Ok(src) => {
            // Write raw bytes — the output may not be UTF-8.
            use std::io::Write;
            let _ = std::io::stdout().write_all(&transpile(&src));
            0
        }
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            1
        }
    }
}

fn cmd_build(args: &[String]) -> i32 {
    let (Some(src_dir), Some(out_dir)) = (args.first(), args.get(1)) else {
        eprintln!("error: `build` needs <src_dir> <out_dir>");
        return 2;
    };
    let src_dir = Path::new(src_dir);
    let out_dir = Path::new(out_dir);

    let mut sources = Vec::new();
    if let Err(e) = collect_sources(src_dir, &mut sources) {
        eprintln!("error: walking {}: {e}", src_dir.display());
        return 1;
    }

    let manifest_path = out_dir.join(".pxp-cache");
    let manifest = read_manifest(&manifest_path);
    let mut next_manifest = HashMap::new();

    let started = Instant::now();
    let (mut transpiled, mut cached, mut bytes_in) = (0usize, 0usize, 0usize);

    for src_path in &sources {
        let rel = src_path.strip_prefix(src_dir).unwrap_or(src_path);
        let src = match std::fs::read(src_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: reading {}: {e}", src_path.display());
                return 1;
            }
        };
        bytes_in += src.len();
        let hash = hash_str(&src);
        let rel_key = rel.to_string_lossy().into_owned();
        let out_path = out_dir.join(rel);

        if manifest.get(&rel_key) == Some(&hash) && out_path.exists() {
            cached += 1;
            next_manifest.insert(rel_key, hash);
            continue;
        }

        let (out, map) = transpile_with_map(
            &src,
            src_path.to_string_lossy(),
            out_path.to_string_lossy(),
        );
        if let Some(parent) = out_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: mkdir {}: {e}", parent.display());
                return 1;
            }
        }
        if let Err(e) = std::fs::write(&out_path, out) {
            eprintln!("error: writing {}: {e}", out_path.display());
            return 1;
        }
        // Sidecar source map next to the generated file, discovered by convention.
        let map_path = append_ext(&out_path, "map");
        if let Err(e) = std::fs::write(&map_path, map.serialize()) {
            eprintln!("error: writing {}: {e}", map_path.display());
            return 1;
        }
        transpiled += 1;
        next_manifest.insert(rel_key, hash);
    }

    write_manifest(&manifest_path, &next_manifest);

    let elapsed = started.elapsed();
    let mb_per_s = if elapsed.as_secs_f64() > 0.0 {
        (bytes_in as f64 / 1e6) / elapsed.as_secs_f64()
    } else {
        0.0
    };
    eprintln!(
        "pxp build: {transpiled} transpiled, {cached} cached ({total} files, {kb} KiB) in {ms:.1}ms  [{mb_per_s:.0} MB/s]",
        total = sources.len(),
        kb = bytes_in / 1024,
        ms = elapsed.as_secs_f64() * 1000.0,
    );
    0
}

/// Differential test against nikic/PHP-Parser: compare our structural node counts
/// (declarations, closures) to the reference's for every corpus file. A count
/// mismatch flags a silent mis-parse (wrong tree, no error).
/// Usage: diff-nikic <dir> <autoload.php> <nikic_counts.php> [n]
fn cmd_diff_nikic(args: &[String]) -> i32 {
    let (Some(dir), Some(autoload), Some(script)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("error: diff-nikic <dir> <autoload.php> <nikic_counts.php> [n]");
        return 2;
    };
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);

    let mut files = Vec::new();
    if let Err(e) = collect_sources(Path::new(dir), &mut files) {
        eprintln!("error: walking {dir}: {e}");
        return 1;
    }
    files.truncate(n);

    // Our counts, keyed by path.
    let mut ours: std::collections::HashMap<String, [u32; 11]> = std::collections::HashMap::new();
    for path in &files {
        let Ok(src) = std::fs::read(path) else { continue };
        let (program, _, _) = parse_stats(&src);
        ours.insert(path.display().to_string(), pxp::astcount::count(&program).as_row());
    }

    // Hand nikic the file list via a temp file (avoids a stdin/stdout pipe deadlock).
    let list_path = std::env::temp_dir().join("pxp-difflist.txt");
    let list = files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    if let Err(e) = std::fs::write(&list_path, list) {
        eprintln!("error: writing list: {e}");
        return 1;
    }
    let output = match std::process::Command::new("php")
        .arg(script)
        .arg(autoload)
        .arg(&list_path)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: running php: {e}");
            return 1;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    let (mut compared, mut nikic_err, mut diverged) = (0usize, 0usize, 0usize);
    let mut examples: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Some((path, rest)) = line.split_once('\t') else { continue };
        if rest == "ERR" {
            nikic_err += 1;
            continue;
        }
        let theirs: Vec<u32> = rest.split(',').filter_map(|x| x.parse().ok()).collect();
        let Some(our) = ours.get(path) else { continue };
        if theirs.len() != 11 {
            continue;
        }
        compared += 1;
        if our[..] != theirs[..] {
            diverged += 1;
            if examples.len() < 20 {
                let diffs: Vec<String> = pxp::astcount::Counts::LABELS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| our[*i] != theirs[*i])
                    .map(|(i, l)| format!("{l}: ours={} nikic={}", our[i], theirs[i]))
                    .collect();
                examples.push(format!("{path}\n      {}", diffs.join(", ")));
            }
        }
    }

    println!(
        "diff-nikic: compared {compared} files\n  nikic parse errors (skipped): {nikic_err}\n  diverged: {diverged}"
    );
    for e in &examples {
        println!("    {e}");
    }
    if diverged == 0 {
        0
    } else {
        1
    }
}

/// Report allocation behavior for transpiling one file: allocation count, total
/// bytes churned, and peak simultaneously-live bytes (the AST's real footprint).
fn cmd_mem_stats(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("error: `mem-stats` needs a file");
        return 2;
    };
    let src = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    #[cfg(not(feature = "memprof"))]
    {
        let _ = &src;
        eprintln!("mem-stats needs the counting allocator: rebuild with `--features memprof`");
        return 2;
    }
    #[cfg(feature = "memprof")]
    {
        use memprof::*;
        // Warm up (page in code paths), then measure a single clean transpile.
        std::hint::black_box(transpile(&src));

        let base_live = LIVE_BYTES.load(Relaxed);
        PEAK_BYTES.store(base_live, Relaxed);
        let (a0, b0) = (ALLOCS.load(Relaxed), ALLOC_BYTES.load(Relaxed));

        let out = transpile(&src);

        let allocs = ALLOCS.load(Relaxed) - a0;
        let churned = ALLOC_BYTES.load(Relaxed) - b0;
        let peak = PEAK_BYTES.load(Relaxed).saturating_sub(base_live);
        std::hint::black_box(&out);

        let kb = |n: usize| n as f64 / 1024.0;
        let file = src.len();
        println!(
            "mem-stats {path}\n  file size:        {:.1} KiB\n  allocations:      {allocs}\n  bytes churned:    {:.1} KiB ({:.1}x file)\n  peak live (AST):  {:.1} KiB ({:.1}x file)\n  alloc per KiB src: {:.0}",
            kb(file),
            kb(churned),
            churned as f64 / file as f64,
            kb(peak),
            peak as f64 / file as f64,
            allocs as f64 / kb(file),
        );
        0
    }
}

/// Robustness fuzzing: deterministically mutate corpus files and assert the full
/// `transpile` pipeline never panics on any input. Mutations: truncation at token
/// boundaries (EOF mid-construct), single-token deletion (broken token streams),
/// and ASCII byte substitution with structurally-interesting bytes.
fn cmd_fuzz(args: &[String]) -> i32 {
    const INTERESTING: &[u8] = b"{}()[]<>$\"'?;:=&|.\\/*# \n";
    let Some(dir) = args.first() else {
        eprintln!("error: `fuzz` needs a directory");
        return 2;
    };
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500);

    let mut files = Vec::new();
    if let Err(e) = collect_sources(Path::new(dir), &mut files) {
        eprintln!("error: walking {dir}: {e}");
        return 1;
    }
    files.truncate(n);
    std::panic::set_hook(Box::new(|_| {}));

    let (mut cases, mut panics) = (0usize, Vec::<String>::new());
    let started = Instant::now();

    let check = |label: String, input: &[u8], cases: &mut usize, panics: &mut Vec<String>| {
        *cases += 1;
        let owned = input.to_vec();
        if std::panic::catch_unwind(|| {
            pxp::transpile(&owned);
        })
        .is_err()
        {
            panics.push(label);
        }
    };

    for path in &files {
        let Ok(src) = std::fs::read(path) else { continue };
        if src.is_empty() {
            continue;
        }
        // The pipeline is byte-oriented, so mutations can hit any byte offset.
        let toks = pxp::lexer::lex(&src);
        let stride = (toks.len() / 64).max(1);

        // 1. Truncation at token boundaries.
        for t in toks.iter().step_by(stride) {
            let end = t.span.end.min(src.len());
            check(format!("truncate@{end} {}", path.display()), &src[..end], &mut cases, &mut panics);
        }
        // 2. Single-token deletion.
        for t in toks.iter().step_by(stride) {
            if t.span.end <= src.len() {
                let mut m = Vec::with_capacity(src.len());
                m.extend_from_slice(&src[..t.span.start]);
                m.extend_from_slice(&src[t.span.end..]);
                check(format!("del-token@{} {}", t.span.start, path.display()), &m, &mut cases, &mut panics);
            }
        }
        // 3. Deterministic byte substitution (LCG — reproducible, no RNG).
        let mut x = 0x9e3779b97f4a7c15u64 ^ src.len() as u64;
        for _ in 0..48 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let pos = (x >> 33) as usize % src.len();
            let mut m = src.clone();
            m[pos] = INTERESTING[(x as usize >> 3) % INTERESTING.len()];
            check(format!("subst@{pos} {}", path.display()), &m, &mut cases, &mut panics);
        }
    }
    let _ = std::panic::take_hook();

    println!(
        "fuzz: {cases} mutations across {} files in {:.1}s\n  panics: {}",
        files.len(),
        started.elapsed().as_secs_f64(),
        panics.len()
    );
    for p in panics.iter().take(15) {
        println!("    PANIC {p}");
    }
    if panics.is_empty() {
        0
    } else {
        1
    }
}

/// Print the first parse errors for one file, with the offending source text.
fn cmd_parse_debug(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("error: `parse-debug` needs a file");
        return 2;
    };
    let src = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let (_program, errors, unknowns) = parse_stats(&src);
    println!("{} errors, {unknowns} unknown nodes", errors.len());
    for e in errors.iter().take(20) {
        let text = &src[e.span.start..e.span.end.min(src.len())];
        let line = src[..e.span.start].iter().filter(|&&b| b == b'\n').count() + 1;
        let snippet: String = String::from_utf8_lossy(text).chars().take(30).collect();
        println!("  L{line}: {} — at {snippet:?}", e.message);
    }
    0
}

/// Parse every `.php` file under a directory and report grammar coverage:
/// panics, parse errors, and `Unknown` recovery nodes. A corpus-driven metric
/// for completing the parser.
fn cmd_parse_check(args: &[String]) -> i32 {
    let Some(dir) = args.first() else {
        eprintln!("error: `parse-check` needs a directory");
        return 2;
    };
    let limit: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);

    let mut files = Vec::new();
    if let Err(e) = collect_sources(Path::new(dir), &mut files) {
        eprintln!("error: walking {dir}: {e}");
        return 1;
    }
    files.truncate(limit);

    // Silence per-file panic spam; we tally them instead.
    std::panic::set_hook(Box::new(|_| {}));

    let (mut panicked, mut with_errors, mut total_errors, mut total_unknowns) = (0, 0, 0usize, 0usize);
    let mut worst: Vec<(usize, String)> = Vec::new();
    let started = Instant::now();

    for path in &files {
        let Ok(src) = std::fs::read(path) else { continue };
        match std::panic::catch_unwind(|| parse_stats(&src)) {
            Ok((_program, errors, unknowns)) => {
                total_errors += errors.len();
                total_unknowns += unknowns;
                if !errors.is_empty() {
                    with_errors += 1;
                }
                let score = errors.len() + unknowns;
                if score > 0 {
                    worst.push((score, path.display().to_string()));
                }
            }
            Err(_) => {
                panicked += 1;
                worst.push((100_000, format!("PANIC {}", path.display())));
            }
        }
    }
    let _ = std::panic::take_hook();

    worst.sort_by(|a, b| b.0.cmp(&a.0));
    let elapsed = started.elapsed();
    println!(
        "parse-check: {} files in {:.1}s\n  panics:        {panicked}\n  files w/errors:{with_errors}\n  parse errors:  {total_errors}\n  Unknown nodes: {total_unknowns}",
        files.len(),
        elapsed.as_secs_f64(),
    );
    if !worst.is_empty() {
        println!("  worst offenders:");
        for (score, path) in worst.iter().take(15) {
            println!("    {score:>6}  {path}");
        }
    }
    0
}

fn cmd_trace(args: &[String]) -> i32 {
    let (Some(gen_file), Some(line_str)) = (args.first(), args.get(1)) else {
        eprintln!("error: `trace` needs <generated.php> <line>");
        return 2;
    };
    let Ok(line) = line_str.parse::<u32>() else {
        eprintln!("error: `{line_str}` is not a line number");
        return 2;
    };

    let map_path = append_ext(Path::new(gen_file), "map");
    let text = match std::fs::read_to_string(&map_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: no source map at {}: {e}", map_path.display());
            return 1;
        }
    };
    let Some(map) = SourceMap::parse(&text) else {
        eprintln!("error: malformed source map at {}", map_path.display());
        return 1;
    };

    println!("{}:{}", map.source_path, map.source_line(line));
    0
}

/// Append an extra extension, e.g. `foo.php` -> `foo.php.map` (not `foo.map`).
fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SOURCE_EXTS.contains(&e))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn hash_str(s: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn read_manifest(path: &Path) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            if let Some((hash, rel)) = line.split_once('\t') {
                if let Ok(h) = hash.parse::<u64>() {
                    map.insert(rel.to_string(), h);
                }
            }
        }
    }
    map
}

fn write_manifest(path: &Path, map: &HashMap<String, u64>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = String::new();
    for (rel, hash) in map {
        body.push_str(&format!("{hash}\t{rel}\n"));
    }
    let _ = std::fs::write(path, body);
}
