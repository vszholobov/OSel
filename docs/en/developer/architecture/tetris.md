---
description: Pinned Go Tetris, single-player launch, external clear and end-to-end keyboard checks.
---

# Tetris

**Audience:** developers changing the game, build or scenario test.

**Summary:** OSel builds `apps/tetris` and runs `/bin/tetris --single`. It needs no network, reads `/dev/tty` and launches external `/bin/clear`. Check with `cargo test --test tetris` in both boot modes.

## Behavior {#how}

The [Tetris](https://github.com/vszholobov/Tetris) submodule pins a commit; [PR 16](https://github.com/vszholobov/Tetris/pull/16) adds single-player. Initialize clones with `git submodule update --init --recursive`.

`lib/field` serves local and server games; `lib/single` handles falling, locking, line clearing and game end. No arguments selects network mode; `--single` selects local play and `--seed N` makes pieces reproducible.

A goroutine reads `/dev/tty` through go-tty; `Raw()` disables canonical input, echo and terminal signals, so Ctrl+C arrives as a byte. Another runtime goroutine serves the fall timer. Restore original terminal modes on exit.

Linux/macOS run external clear; Windows uses `cmd /c cls`. OSel finds `/bin/clear` through PATH. Launch failure ends the client with a message and nonzero status. Clear runs once on startup.

The field/preview use `#`, requiring no Cyrillic. ANSI H positions the cursor, ?25l/?25h request visibility. Controls are a/d, s, q/e; x/Escape/Ctrl+C exit. Input queue is bounded to 64 characters. Fall and redraw timers are separate; redraw is capped at 30 Hz and only occurs on changes. `singleRenderer` compares with the last successfully written frame, emits changed spans and erases old cells with spaces. Repeated wall input produces no redraw; multiple actions can merge into one frame.

## Build and run {#run}

See [runner](../reference/runner.md#tetris). Shell launches Tetris with `--single`, `PATH=/bin`, `TERM=xterm`, `GODEBUG=asyncpreemptoff=1`, then returns to `osel>` for repeat play.

## Code {#code}

`.gitmodules` and `apps/tetris` pin the game. `runner/src/main.rs` provides `build_tetris` and `TetrisInput`; `src/shell.rs` launches it; `tests/tetris.rs` runs the test kernel.

## Invariants and pitfalls {#pitfalls}

Update gitlinks explicitly; CI initializes submodules recursively. Missing submodule with installed Go fails. Missing Go emits `TETRIS SKIP: Go is not installed`, allowed only when runner did not build a binary; a missing built executable must fail. CI installs Go, making the test mandatory. Async Go preemption is disabled until signals exist, but hardware scheduling remains enabled. Test mode strips ANSI logs; Run forwards them to display the game on both framebuffer and host terminal.

## Tests {#tests}

Game `go test -race ./...` in lib/client checks movement, walls, rotations, lines, score, reproducibility, end state and frames. Linux/Windows/macOS cross-builds do not substitute for actual runs. Renderer regressions cover seven shapes, partial/full equivalence, unchanged-frame silence, short move output, retry after write failure and falling/exit under continuous input.

`tests/tetris.rs` uses `--single --seed 2 --trace`, starting with an L piece so rotation changes state. Runner requires TETRIS READY and an unprompted TETRIS TICK to prove blocking reads do not stop the timer. It sends a,d,s,q,e and requires `changed=true` after each, then x and TETRIS EXIT. Exit code alone is insufficient. The framework checks heap/frame recovery. Runner tests verify ordering, duplicate/unrelated lines, ANSI filtering and missing-Go skips in both firmware modes.

_Checked against code: 2026-10-13, working branch for stage 3._
