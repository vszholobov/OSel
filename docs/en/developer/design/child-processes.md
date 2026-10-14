---
description: Completed stage 2.5 — Go child processes, execve, waiting and external clear.
---

# Child programs from Go

**Audience:** developers looking for stage 2.5 results.

**Summary:** [PR 9](https://github.com/vszholobov/OSel/pull/9) implements Go `os/exec`, including `/bin/clear`. See [processes](../architecture/core/threads.md#child-processes), [pipes](../architecture/core/fs.md#pipes), [calls](../reference/syscalls.md) and [0017](../decisions/0017-child-processes.md). [Tetris](../architecture/tetris.md) retains its external clear invocation.
