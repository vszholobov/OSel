---
description: Pinned Rust nightly, upgrade procedure and GitHub Actions checks.
---

# Toolchain and CI

**Audience:** developers updating the compiler or investigating CI.

**Summary:** `rust-toolchain.toml` pins nightly. Update it deliberately after both latest-nightly jobs pass. PR CI checks host tests, documentation, both QEMU boot modes, the real kernel and the documentation site; master publishes the site.

## Toolchain {#toolchain}

The kernel uses unstable `custom_test_frameworks` and `abi_x86_interrupt`. rustup installs the pinned nightly, `rust-src`, `llvm-tools-preview` and `x86_64-unknown-none` automatically. See [0004](../decisions/0004-pinned-nightly.md).

## Update nightly {#bump}

1. Run GitHub **Actions** → **test** → **Run workflow** on master. `latest nightly (uefi)` and `latest nightly (bios)` also run every Monday.
2. If both pass, read `rustc -V` from “Install the latest nightly”.
3. Set `channel = "nightly-YYYY-MM-DD"` in `rust-toolchain.toml`. The channel date is one day later than the compiler date printed by `rustc -V`.
4. Open a PR for the update and let regular CI verify it.

If latest nightly fails, keep the pinned version and investigate before updating.

## CI jobs {#jobs}

| Workflow | Job | Trigger and checks |
|---|---|---|
| `.github/workflows/test.yml` | `host unit tests` | PR/master: core, runner and documentation checks |
| `test.yml` | `cargo test (uefi)`, `cargo test (bios)` | After host tests: QEMU suite and `OSEL_EXPECT="OSel ready to go!" cargo run`; installs Go for end-to-end programs |
| `test.yml` | `latest nightly (uefi)`, `latest nightly (bios)` | Weekly/manual: core, runner and QEMU tests on latest nightly; no docs or real-kernel check; independent of host job |
| `.github/workflows/docs.yml` | `build the site` | PR/master: Diplodoc builds both languages, then `tools/docs-site.py` checks pages and generates legacy redirects |
| `docs.yml` | `publish to GitHub Pages` | Master push/manual: publish the built site |
| `.github/workflows/release.yml` | `build and verify release images` | PR or `v*` tag: release-profile images, verified in both firmware modes with required Tetris |
| `release.yml` | `publish the verified images` | After both tagged images pass: GitHub Release and SHA-256 sums; see [releases](release.md#images) |

New PR pushes cancel stale PR runs; master runs are never canceled.

### Cache {#cache}

`Swatinem/rust-cache` caches the root project, `runner/` and `tools/docs-check/`. OVMF is cached separately in `runner/.ovmf`, keyed by `runner/Cargo.lock`.

## Documentation checks {#docs-check}

`cargo docs-check` checks both locales for modules, runner environment variables, handlers and locks; it verifies code paths, Markdown targets, anchors, per-language tables of contents and matching page/anchor sets. See [documentation rules](writing-docs.md).

## Common failures {#errors}

| Failure | Action |
|---|---|
| Only latest nightly is red | Regular CI is unaffected; investigate before the next update |
| Timer test fails only in CI | See [symptoms](troubleshooting.md#symptoms) |
| `failed to fetch OVMF firmware` | Retry a temporary network failure |
| Site build rejects a link | Fix the path or add the page to that locale's toc |
| `Docs match the code` fails | The assertion names the missing detail and page |
