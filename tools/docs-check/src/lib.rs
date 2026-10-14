//! Helpers for the docs checks in `tests/drift.rs`: plain string scanning, no dependencies.

use std::fs;
use std::path::{Path, PathBuf};

/// The repository root
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

pub fn read(path: &str) -> String {
    fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Source without its `#[cfg(test)]` part: test modules always come last in a file
pub fn without_tests(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

/// Every Rust source file below `dir`, relative to the repository, sorted.
pub fn rust_files(dir: &str) -> Vec<String> {
    fn walk(root: &Path, path: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(path).unwrap_or_else(|e| panic!("cannot list {}: {e}", path.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() { walk(root, &path, out); }
            else if path.extension().is_some_and(|e| e == "rs") { out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")); }
        }
    }
    let root = repo(); let mut paths = Vec::new(); walk(&root, &root.join(dir), &mut paths); paths.sort(); paths
}
fn module_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap().to_str().unwrap();
    if stem == "mod" { path.parent().unwrap().file_name().unwrap().to_str().unwrap().into() } else { stem.into() }
}
/// Module names, including directory modules and their driver implementations.
pub fn modules(dir: &str, except: &[&str]) -> Vec<String> {
    let mut names: Vec<_> = rust_files(dir).iter().map(|p| module_name(Path::new(p)))
        .filter(|name| !except.contains(&name.as_str())).collect();
    names.sort(); names.dedup(); names
}

/// Every `.md` file below `dir` (relative to the repository), recursively, sorted
pub fn markdown_files(dir: &str) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let root = repo();
    let mut out = Vec::new();
    walk(&root, &root.join(dir), &mut out);
    out.sort();
    out
}

/// Words made of `[A-Za-z0-9_]` that follow `prefix`, e.g. `idt.` -> `page_fault`
pub fn identifiers_after(text: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (start, _) in text.match_indices(prefix) {
        let rest = &text[start + prefix.len()..];
        let word: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !word.is_empty() && !found.contains(&word) {
            found.push(word);
        }
    }
    found
}

/// String literals passed as the first argument after `prefix`, e.g. `IrqSpinlock::new("` -> `WRITER`
pub fn string_arguments(text: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (start, _) in text.match_indices(prefix) {
        let rest = &text[start + prefix.len()..];
        if let Some(end) = rest.find('"') {
            let name = rest[..end].to_string();
            if !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// Inline code spans of a Markdown text, skipping fenced code blocks
pub fn code_spans(markdown: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut parts = line.split('`');
        parts.next();
        while let (Some(code), Some(_)) = (parts.next(), parts.next()) {
            spans.push(code.to_string());
        }
    }
    spans
}

/// Code spans that name a file or folder of the repository: `src/memory.rs`,
/// `src/sync.rs::IrqSpinlock`, `osel-core/src/`. Generated and wildcard paths are skipped.
pub fn repo_paths(markdown: &str) -> Vec<String> {
    const ROOTS: &[&str] = &[
        "src/",
        "osel-core/",
        "runner/",
        "tests/",
        "tools/",
        "docs/",
        ".github/",
        ".cargo/",
        ".claude/",
    ];
    const FILES: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "AGENTS.md",
        "CLAUDE.md",
        "CHANGELOG.md",
        "readme.md",
    ];
    code_spans(markdown)
        .into_iter()
        .filter_map(|span| {
            // `src/sync.rs::IrqSpinlock` and `docs/ru/developer/guides/debugging.md#debugger` name a file too
            let path = span
                .split("::")
                .next()?
                .split('#')
                .next()?
                .trim()
                .to_string();
            let is_repo_path =
                ROOTS.iter().any(|root| path.starts_with(root)) || FILES.contains(&path.as_str());
            let generated = path.contains('*')
                || path.contains('<')
                || path.contains(' ')
                || path.starts_with("runner/.ovmf");
            (is_repo_path && !generated).then_some(path)
        })
        .collect()
}

/// Targets of Markdown links `[text](target)` outside fenced code blocks
pub fn link_targets(markdown: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for (start, _) in line.match_indices("](") {
            let rest = &line[start + 2..];
            if let Some(end) = rest.find(')') {
                targets.push(rest[..end].trim().to_string());
            }
        }
    }
    targets
}

/// Explicit heading anchors `{#id}` defined in a page
pub fn anchors(markdown: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let line = line.trim_start();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('#') {
            continue;
        }
        for (start, _) in line.match_indices("{#") {
            let rest = &line[start + 2..];
            if let Some(end) = rest.find('}') {
                ids.push(
                    rest[..end]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string(),
                );
            }
        }
    }
    ids
}

/// HTML-like angle brackets in prose, including unquoted Rust generics and placeholders.
/// The full Markdown linter runs in CI; this catches accidental tags before that build.
pub fn unquoted_tags(markdown: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut in_fence = false;
    for (number, line) in markdown.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for prose in line.split('`').step_by(2) {
            for (start, _) in prose.match_indices('<') {
                let rest = &prose[start + 1..];
                let name = rest.trim_start_matches('/');
                if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    continue;
                }
                if let Some(end) = rest.find('>') {
                    if !rest[..end].contains("://") {
                        found.push((number + 1, prose[start..=start + end + 1].to_string()));
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_identifiers() {
        let text = "idt.page_fault.set(); idt.debug; idt[InterruptIndex::Timer.as_u8()]";
        assert_eq!(identifiers_after(text, "idt."), ["page_fault", "debug"]);
        assert_eq!(identifiers_after(text, "InterruptIndex::"), ["Timer"]);
    }

    #[test]
    fn finds_string_arguments() {
        let text = r#"IrqSpinlock::new("WRITER", None); IrqSpinlock::new("PICS", x); IrqSpinlock::new("WRITER", y)"#;
        assert_eq!(
            string_arguments(text, "IrqSpinlock::new(\""),
            ["WRITER", "PICS"]
        );
    }

    #[test]
    fn code_spans_skip_fences() {
        let text = "see `src/a.rs` and `x`\n```\n`src/in_fence.rs`\n```\nthen `tests/b.rs::name`";
        assert_eq!(code_spans(text), ["src/a.rs", "x", "tests/b.rs::name"]);
        assert_eq!(repo_paths(text), ["src/a.rs", "tests/b.rs"]);
    }

    #[test]
    fn finds_links_and_anchors() {
        let page = "# Title {#top}\n[a](b.md#part) and [c](https://x.org)\n```\n[no](fence.md)\n```\n## Part {#part}";
        assert_eq!(link_targets(page), ["b.md#part", "https://x.org"]);
        assert_eq!(anchors(page), ["top", "part"]);
    }

    #[test]
    fn anchor_examples_do_not_define_sections() {
        let page = "Text `{#inline}`\n```markdown\n## Example {#example}\n```\n## Real {#real}";
        assert_eq!(anchors(page), ["real"]);
    }

    #[test]
    fn unquoted_generics_and_paths_are_not_markdown_code() {
        let page = "Use Arc<Process> and bin/<name>.\n`Arc<Process>` is code.\n```rust\nArc<Process>\n```\n<https://example.com>";
        assert_eq!(
            unquoted_tags(page),
            [(1, "<Process>".to_string()), (1, "<name>".to_string())]
        );
    }

    #[test]
    fn repo_paths_skip_generated_and_patterns() {
        let text = "`src/**` `runner/.ovmf` `osel-core/src/` `Cargo.toml` `cargo test` `<path>` `docs/a.md#part`";
        assert_eq!(
            repo_paths(text),
            ["osel-core/src/", "Cargo.toml", "docs/a.md"]
        );
    }
}

#[cfg(test)]
mod source_tree_tests {
    use super::*;
    #[test] fn directory_modules_keep_their_names() {
        assert_eq!(module_name(Path::new("src/drivers/mod.rs")), "drivers");
        assert_eq!(module_name(Path::new("src/drivers/e1000e.rs")), "e1000e");
        assert!(rust_files("src").iter().any(|p| p == "src/drivers/pci.rs"));
    }
}
