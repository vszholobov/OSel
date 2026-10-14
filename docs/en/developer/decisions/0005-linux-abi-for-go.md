---
description: Why OSel implements a Linux system-call subset for static Go programs.
---

# 0005. Linux ABI for Go programs

**Audience:** developers designing system calls and user mode.

**Summary:** the goal is console Tetris written in Go. Build an ordinary Linux binary with `GOOS=linux GOARCH=amd64 CGO_ENABLED=0`; implement the Linux call numbers and conventions needed by its runtime. Unsupported calls return `ENOSYS`.

**Status:** accepted. **Date:** 2026-10-11.

## Context

Go calls the kernel directly without libc. A static Linux binary depends only on the kernel interface.

## Decision

Use Linux x86_64 call numbers, registers and semantics. Construct the initial stack according to System V ABI with argv, envp and auxv. Implement the subset needed by Go and Tetris; see [stage 2](../design/stage-2-user-mode.md).

## Alternatives considered

A custom system-call interface would require porting the Go runtime. Rust programs using a custom interface would not satisfy the existing Go Tetris goal.

## Consequences

The Go runtime drives thread support through `clone` and `futex`, TLS through `arch_prctl`, and timers through `epoll`. End-to-end OSel program tests check compatibility; automated comparison with Linux remains technical debt.
