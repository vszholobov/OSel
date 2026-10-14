---
description: OSel developer documentation — building, tests, architecture, reference, decisions and plans.
---

# Developer documentation

Language: [Русский](../../ru/developer/index.md) · [English](index.md)

**Audience:** anyone changing the kernel, programs, runner or documentation.

**Summary:** start with the build instructions and module map. Architecture pages describe OSel today, decisions explain the reasons, and plans separate unfinished work.

## Getting started {#start}

1. [Build and run](guides/quickstart.md).
2. [Read the module map](architecture/overview.md).
3. [Add and run a test](guides/testing.md).

## Find what you need {#sections}

| Task | Section |
|---|---|
| Understand the kernel | [Architecture overview](architecture/overview.md) |
| Investigate a crash | [Debugging](guides/debugging.md), [troubleshooting](guides/troubleshooting.md) |
| Find launch settings | [Runner](reference/runner.md) |
| Add a system call | [Linux calls](reference/syscalls.md), [user mode](architecture/core/user-mode.md) |
| Understand a choice | [Our decisions](decisions/index.md) |
| Choose the next task | [Roadmap](roadmap.md), [technical debt](debt.md) |
| Update the compiler or CI | [Toolchain and CI](guides/toolchain-and-ci.md) |
| Release an image | [Releases](guides/release.md) |
| Update both language versions | [Documentation rules](guides/writing-docs.md) |

For ready-made images, use the [user documentation](../user/index.md).
