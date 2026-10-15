---
description: Maintain CHANGELOG.md and release verified UEFI/BIOS images through GitHub Actions.
---

# Changelog and releases

**Audience:** developers opening PRs or releasing OSel.

**Summary:** every PR adds an entry to `CHANGELOG.md` under «Не выпущено» (Unreleased). A release numbers that section and tags the commit `vX.Y`. CI builds and verifies both images and publishes them with checksums. Documentation tracks master; history belongs in the changelog.

## Changelog {#changelog}

Keep Unreleased at the top, followed by releases newest first. Every PR, including documentation or tests, adds a reader-facing entry to one group: «Добавлено» (Added), «Изменено» (Changed), «Исправлено» (Fixed), or «Удалено» (Removed). Describe the effect on users or developers.

## Version numbers {#versions}

Starting with `0.11`, public versions use `X.Y`, such as `0.11`, with tags such as `v0.11`. While the major version is zero, compatibility is not promised. Increment `Y` for each release; release numbers are no longer tied to roadmap stages.

Cargo requires three components, so `version` in `Cargo.toml` stores `X.Y.0`: release `0.11` uses `0.11.0`. The last component is always zero. The kernel prints this build version and commit at boot and on panic; release notes identify both version forms. The previous `v0.1.0` release and changelog entry stay unchanged.

## Release procedure {#release}

1. Ensure master CI is green.
2. Rename Unreleased to `## X.Y — YYYY-MM-DD`, without square brackets, and add a new empty Unreleased section above it. Prepare a concise English changelog for the released version; it becomes the public GitHub Release description.
3. Set `version = "X.Y.0"` in `Cargo.toml`; update the root package in `Cargo.lock` with `cargo check`. Commit pinned dependencies and the `apps/tetris` gitlink.
4. Merge a PR, then tag the merge commit:

   ```shell
   git switch master
   git pull --ff-only origin master
   git tag vX.Y
   git push origin vX.Y
   ```

5. Wait for **release images**. [GitHub Releases](https://github.com/vszholobov/OSel/releases) must contain `OSel.uefi.img`, `OSel.bios.img` and `SHA256SUMS`, plus version and commit. Images are uncompressed; users follow [USB boot](../../user/usb.md#image).
6. Mark completed stages in the [roadmap](../roadmap.md).

## Image verification and publication {#images}

`.github/workflows/release.yml` runs for `v*` tags. `tools/release.py` accepts only `vX.Y`, checks its `X.Y.0` Cargo version, tagged commit and a nonempty dated `X.Y` changelog section, and uses its concise English changelog as release text. The generated version/source metadata and image download instructions are also in English.

Both release-profile images come from one checkout and kernel ELF. Rust is pinned in `rust-toolchain.toml`; Go is pinned in the workflow. Every Cargo build, including nested runner/program builds, uses `--locked`; Go uses `-mod=readonly`, module versions/checksums and the fixed Tetris submodule. Tools are installed on GitHub Actions machines.

For each boot mode, runner first checks readiness. Then `tools/check-shell.py --image … --boot … --require-tetris --expected-version … --expected-commit …` launches the copied image directly. It verifies the banner, shell, `hello`, external `clear`, font license, mandatory Tetris, two game runs, timer/movement/exit and COM1/PS2 input. Disk and UEFI-variable snapshots prevent modifying release files.

Create `SHA256SUMS` immediately after building and recheck after both image tests. Upload artifacts only after success. Publication downloads artifacts and rechecks sums, creates a draft with three assets, then makes it public only after every upload succeeds. PR builds run the same verification but do not publish.

## Verify a published release {#verify}

Download both images and `SHA256SUMS` to one folder. Run `sha256sum -c SHA256SUMS` on Linux or `shasum -a 256 -c SHA256SUMS` on macOS; both must report `OK`. From the repository root:

```shell
python3 tools/check-shell.py --image /tmp/OSel.bios.img --boot bios --require-tetris
python3 tools/check-shell.py --image /tmp/OSel.uefi.img --boot uefi --require-tetris \
  --ovmf-code runner/.ovmf/x64/code.fd --ovmf-vars runner/.ovmf/x64/vars.fd
```

Use your image paths. Python 3, QEMU and OVMF for UEFI are sufficient; image verification does not invoke Rust or Go. Runner fetches OVMF on its first UEFI run. Add `--expected-version X.Y.0 --expected-commit <short-hash>` from the release description to verify the banner.

## Release failures {#errors}

| Failure | Action |
|---|---|
| Tag/version mismatch or missing changelog section | Correct preparation through a PR and tag the corrected commit |
| Either image fails | Nothing publishes; investigate the QEMU log |
| Asset upload fails | Remove only the failed draft, preserve the tag and use **Re-run all jobs** |
| Release already exists | Existing releases are not overwritten; use a new version |

## No documentation versioning {#no-versioned-docs}

Docs describe current master in both languages. Consult changelog or `docs/` at a release tag for older behavior.
