//! The documentation in docs/ must not drift from the code. Each test names what is
//! missing and on which page it belongs, see docs/ru/developer/guides/writing-docs.md.

use docs_check::*;

fn assert_documented(page: &str, what: &str, names: &[String]) {
    for lang in ["ru", "en"] {
        let page = format!("docs/{lang}/{page}");
        let text = read(&page);
        let missing: Vec<&String> = names
            .iter()
            .filter(|name| !text.contains(&format!("`{name}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "{page} does not mention these {what} (as `name`): {missing:?}"
        );
    }
}

#[test]
fn module_map_lists_every_module() {
    let mut modules = modules("src", &["lib", "main"]);
    modules.extend(modules_of_core());
    assert_documented("developer/architecture/overview.md", "modules", &modules);
}

fn modules_of_core() -> Vec<String> {
    docs_check::modules("osel-core/src", &["lib"])
}

#[test]
fn runner_page_lists_every_env_var() {
    let mut vars = identifiers_after(&read("runner/src/main.rs"), "OSEL_");
    vars.extend(identifiers_after(&read("runner/run.sh"), "OSEL_"));
    let vars: Vec<String> = vars.into_iter().map(|v| format!("OSEL_{v}")).collect();
    assert!(
        !vars.is_empty(),
        "no OSEL_ variables found, has the runner moved?"
    );
    assert_documented("developer/reference/runner.md", "runner variables", &vars);
}

#[test]
fn vectors_page_lists_every_handler() {
    let source = read("src/interrupts.rs");
    let code = without_tests(&source);
    let mut names = identifiers_after(code, "idt.");
    names.extend(identifiers_after(code, "InterruptIndex::"));
    names.retain(|name| name != "as_u8");
    assert!(names.len() > 10, "too few IDT entries found: {names:?}");
    assert_documented("developer/reference/vectors.md", "IDT entries", &names);
}

#[test]
fn syscalls_page_lists_every_call() {
    let source = read("osel-core/src/syscall.rs");
    let table = source
        .split("pub const SYSCALLS")
        .nth(1)
        .expect("no SYSCALLS table in osel-core/src/syscall.rs");
    let table = &table[..table.find("];").expect("the SYSCALLS table does not end")];
    // every second piece between quotes is a name: `(WRITE, "write"), ...`
    let names: Vec<String> = table
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert!(names.len() >= 4, "too few system calls found: {names:?}");
    assert_documented("developer/reference/syscalls.md", "system calls", &names);
}

#[test]
fn locking_page_lists_every_lock() {
    let mut locks = Vec::new();
    for dir in ["src", "osel-core/src"] {
        for module in docs_check::modules(dir, &[]) {
            let source = read(&format!("{dir}/{module}.rs"));
            let code = without_tests(&source);
            for prefix in ["IrqSpinlock::new(\"", "Spinlock::new(\""] {
                for lock in string_arguments(code, prefix) {
                    if !locks.contains(&lock) {
                        locks.push(lock);
                    }
                }
            }
        }
    }
    assert!(!locks.is_empty(), "no locks found, has IrqSpinlock moved?");
    assert_documented("developer/architecture/core/locking.md", "locks", &locks);
}

#[test]
fn code_paths_in_docs_exist() {
    let mut files = markdown_files("docs");
    files.extend(markdown_files(".claude"));
    files.extend(
        ["AGENTS.md", "CLAUDE.md", "readme.md", "CHANGELOG.md"]
            .iter()
            .map(|f| f.to_string()),
    );
    let repo = repo();
    let mut missing = Vec::new();
    for file in &files {
        if !repo.join(file).exists() {
            continue;
        }
        for path in repo_paths(&read(file)) {
            if !repo.join(&path).exists() {
                missing.push(format!("{file}: `{path}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "paths in the docs that don't exist:\n{}",
        missing.join("\n")
    );
}

/// Diplodoc checks that linked pages exist, but not the `#anchor` part of a link: anchors
/// that other pages link to must be explicit `{#id}` (see docs/ru/developer/guides/writing-docs.md)
#[test]
fn links_to_sections_have_explicit_anchors() {
    let repo = repo();
    let mut broken = Vec::new();
    for page in markdown_files("docs") {
        let text = read(&page);
        let dir = std::path::Path::new(&page).parent().unwrap().to_path_buf();
        for target in link_targets(&text) {
            if target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let (file, anchor) = target.split_once('#').unwrap_or((target.as_str(), ""));
            if anchor.is_empty() {
                continue;
            }
            let target_page = if file.is_empty() {
                page.clone()
            } else {
                dir.join(file).to_string_lossy().into_owned()
            };
            let Ok(target_text) = std::fs::read_to_string(repo.join(&target_page)) else {
                broken.push(format!("{page}: ({target}) points to a missing page"));
                continue;
            };
            if !anchors(&target_text).iter().any(|id| id == anchor) {
                broken.push(format!("{page}: ({target}) has no {{#{anchor}}} heading"));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "links to missing sections:\n{}",
        broken.join("\n")
    );
}

#[test]
fn every_page_is_in_the_table_of_contents() {
    for lang in ["ru", "en"] {
        let root = format!("docs/{lang}");
        let toc_path = format!("{root}/toc.yaml");
        let toc = read(&toc_path);
        let hrefs: Vec<String> = toc
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("- href:")
                    .or_else(|| line.trim().strip_prefix("href:"))
            })
            .map(|href| href.trim().trim_matches(['"', '\'']).to_string())
            .collect();
        let pages: Vec<String> = markdown_files(&root)
            .into_iter()
            .map(|page| page.strip_prefix(&format!("{root}/")).unwrap().to_string())
            .filter(|page| !page.starts_with('_') && !page.contains("/_"))
            .collect();
        let missing: Vec<_> = pages.iter().filter(|page| !hrefs.contains(page)).collect();
        assert!(
            missing.is_empty(),
            "pages missing in {toc_path}: {missing:?}"
        );
        let dangling: Vec<_> = hrefs.iter().filter(|href| !pages.contains(href)).collect();
        assert!(
            dangling.is_empty(),
            "{toc_path} points to missing pages: {dangling:?}"
        );
    }
}

#[test]
fn language_versions_have_matching_pages_and_anchors() {
    let relative_pages = |lang: &str| {
        let root = format!("docs/{lang}/");
        markdown_files(&root)
            .into_iter()
            .map(|p| p.strip_prefix(&root).unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let pages = relative_pages("ru");
    assert_eq!(
        pages,
        relative_pages("en"),
        "Russian and English page sets differ"
    );
    for page in pages {
        let mut ru = anchors(&read(&format!("docs/ru/{page}")));
        let mut en = anchors(&read(&format!("docs/en/{page}")));
        ru.sort();
        en.sort();
        assert_eq!(ru, en, "translated section anchors differ in {page}");
    }
}

#[test]
fn documentation_links_resolve() {
    let root = repo();
    for page in markdown_files("docs") {
        let dir = std::path::Path::new(&page).parent().unwrap().to_path_buf();
        for target in link_targets(&read(&page)) {
            if target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let file = target
                .split('#')
                .next()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !file.is_empty() {
                assert!(
                    root.join(&dir).join(file).exists(),
                    "{page}: missing link target {target}"
                );
            }
        }
    }
}
