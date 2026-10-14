---
description: OSel documentation structure, bilingual updates, page templates, links and checks.
---

# Writing documentation

Language: [Русский](../../../ru/developer/guides/writing-docs.md) · [English](writing-docs.md)

**Audience:** everyone updating documentation, including agents.

**Summary:** user and developer documentation exist in Russian and English. Each fact has one home per language. Use page templates, repository-relative code paths and explicit heading anchors; add each new page to both tables of contents. CI builds the site and checks drift.

## Layers and sections {#layers}

Entry points are `docs/ru/index.md` and `docs/en/index.md`, each with `user/index.md` and `developer/index.md`. Ready-made images, gameplay, hardware and crash reporting are user topics. Source builds, testing, kernel internals, reference and decisions are developer topics.

Update translations in the same PR. Page paths, commands, limitations and explicit anchors must match. Russian is the basis for agreeing technical changes; English contains the same material. Add pages to both tables of contents. Explain OSel and our choices without educational-source citations or comparisons with other projects. Preserve third-party licenses.

| Section | Local folder | Purpose |
|---|---|---|
| User | `user/` | Run OSel and report problems |
| Procedures | `developer/guides/` | Complete a concrete development task |
| Architecture | `developer/architecture/` | Current behavior, invariants and pitfalls |
| Reference | `developer/reference/` | Exact values and interfaces |
| Decisions | `developer/decisions/` | Reasons and alternatives |
| Design | `developer/design/` | Proposed work not yet implemented |
| Roadmap/debt | `developer/roadmap.md`, `developer/debt.md` | Future and deferred work |

Agent instructions live outside the site in `AGENTS.md`, `.claude/rules/` and `.claude/skills/`. They route readers to the documentation instead of duplicating it. `.claude/rules/docs.md` directs documentation changes here.

## One home per fact {#single-home}

Current subsystem behavior and invariants belong to its architecture page; traps belong to its pitfalls section. Reasons belong to a new decision. Exact values belong to reference. Steps belong to guides or user documentation. Future designs belong to design with a roadmap checkbox. Deferred work gets one debt row linking its details. Function-specific detail belongs in code comments.

While a stage is active, its design page owns the proposal. The closing PR moves implemented details into architecture and decisions and reduces the design page to completion and result links.

## Page templates {#templates}

Begin each page with metadata and a short introduction:

```markdown
---
description: A one-line description used by search and llms.txt.
---

# Title

**Audience:** …

**Summary:** two or three sentences answering the main question.
```

Architecture pages contain behavior, code locations, invariants, locking/interrupt context, pitfalls, tests, related decisions and the last code review. Procedures contain goal, steps, verification and common errors. Reference pages contain manually maintained tables checked by `cargo docs-check`. Decisions contain status/date, context, decision, alternatives and consequences. Active designs contain stage goal, existing foundation, steps with verification, explicitly marked proposals and open questions.

Accepted decisions retain their technical history: changed behavior requires a new decision and a status update on the old one. Number decisions sequentially and add them to the index. Translation and removal of source references do not change the technical decision.

## Markup {#markup}

Use plain Markdown readable without a build. Diplodoc extensions are limited to notes, expandable details, tabs for mutually exclusive alternatives and includes:

Put angle-bracket types (`Arc<Process>`) and placeholder paths (`bin/<name>`) in backticks. Otherwise the builder treats them as HTML tags and rejects the page: raw HTML is disabled. `cargo docs-check` catches these omissions in both languages.

```markdown
{% note warning %}

Important detail.

{% endnote %}

{% cut "Technical detail" %}

Expandable text.

{% endcut %}

{% list tabs %}

- UEFI

  UEFI steps.

- BIOS

  BIOS steps.

{% endlist %}

{% include [shared text](../_includes/file.md) %}
```

Keep blank lines around opening/closing directives. Included files belong in `_includes/`; an underscore-prefixed folder prevents Diplodoc from discarding them.

## Links {#links}

Use relative `.md` links for pages, optionally with an explicit anchor. Every linked heading must define a stable Latin-letter `{#id}` anchor. Do not change it when translating a heading. Example: `## Stage 2 {#stage-2}`.

Refer to code by repository path and function name without line numbers, such as `src/memory.rs`, `with_memory`. `cargo docs-check` verifies that paths exist. External links are for functional destinations such as releases and tool downloads.

## Names and order {#naming}

Russian text goes in `docs/ru/`, English in `docs/en/`. Matching filenames use English words with hyphens, such as `crash-report.md`. Each locale's `toc.yaml` controls ordering. Pages absent from it do not appear on the site. Only decisions have numeric filename prefixes. Split pages longer than about 300 lines; add an entry page when a folder has five or more pages.

## Checks {#checks}

| Check | Location | Detects |
|---|---|---|
| Diplodoc build | `.github/workflows/docs.yml` | Markup and missing page targets |
| `cargo docs-check` | Local and host CI | Missing modules, runner variables, handlers, locks, code paths, toc entries, targets, explicit anchors and differing translation page/anchor sets |
| Site output check | `tools/docs-site.py` after CI build | Missing rendered pages in either language and missing redirect targets |
| PR template | PR creation | Documentation updated or an explanation why unnecessary |

Do not build the site locally: CI builds every PR and publishes after merging to master. `docs/redirects.json` maps the root and old public URLs to Russian pages. Shared images stay in `docs/images/`, styles in `docs/_assets/`; `docs/.yfm` selects languages. `tools/docs-site.py` generates redirects after checking both language outputs.

## Last code review {#last-reviewed}

End architecture pages with a date and commit:

```markdown
_Checked against code: 2026-10-12, commit ae1b2a6._
```

A subsystem code change updates its documentation and this line in both languages.
