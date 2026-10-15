---
description: Framebuffer console, built-in raster fonts, ANSI rendering and panic screen.
---

# Console

**Audience:** developers changing display output.

**Summary:** OSel Console embeds Terminus-derived 16×32 and fallback 8×16 rasters. Large letters use a native raster rather than scaling. UEFI/BIOS share host-tested rendering in osel-core; kernel avganec owns globals, print macros and panic display.

## Behavior {#how}

Console in `osel-core/src/console.rs` uses a byte buffer and FrameBufferInfo. Cells are 8×16/16×32 with built-in line spacing. Text scrolls at the bottom. Newline advances a line, carriage return starts it, tab advances to a multiple-of-eight position without wrapping. Backspace erases the previous cell but does nothing at line start. Other controls are not drawn. Font coverage is printable ASCII, Ж and replacement �; unsupported characters use UNKNOWN_CHAR. Glyph pixels have sharp foreground/background colors without antialiasing.

new_with_font selects raster; new uses the small font. new_scaled supports integer ×1/×2/×3 of the small raster but kernel does not use it. Cell geometry, ANSI clearing, scrolling and TIOCGWINSZ all account for font/scale; COM1 output is unaffected.

avganec::init selects 16×32 when at least 80×25 fits, otherwise 8×16. The large raster requires at least 1280×800. QEMU's 1280×1024 gives 80×32. UEFI retains firmware selection because bootloader's minimum chooses the last suitable mode (2560×1600 in QEMU); BIOS requests 1280×800. Lookup occurs once per glyph; spaces fill cells directly; all glyph cells overwrite spacing. Clearing combines horizontal spans and fills uniform colors efficiently.

Console::new rejects unknown formats, bad pixel size/stride, a screen too small for one cell or a short buffer with ConsoleError.

ANSI parsing survives writes: CSI A/B/C/D, H/f, G move the cursor; J/K erase; coordinates clamp to screen. SGR supports basic/bright colors and reset; other attributes are consumed without rendering. ?25h/l records visibility but no text cursor is drawn yet. At most eight 16-bit parameters are allowed; overflow saturates and excess parameters invalidate a sequence. External clear emits ESC[H ESC[2J; console code does not launch programs.

Kernel init consumes framebuffer and reports failure to serial; screen printing before init/without framebuffer is a no-op. try_print avoids waiting for exception output. with_console/read_pixel support inspection; panic_screen draws failures.

## Scrolling in RAM {#scrolling}

After heap initialization, `avganec::init_text_buffer` enables `Console::enable_buffering`. Arrays in ordinary RAM store desired/displayed characters and colors. A saved screen preserves the shell during fullscreen programs; a ring retains SCROLLBACK_LINES = 1024 scrolled-off rows, with one extra cell per row for the right margin and separate bottom-margin state. Storage follows cell count: roughly 1.04 MiB for screens and history at 80×32, versus 5 MiB for a single 1280×1024 pixel shadow. Output performs no allocation.

Wrapping, scrolling and ANSI update cells. `flush_row` redraws only differences; intermediate screens within an output chunk are skipped. Glyph scanlines are composed on the stack and written contiguously. This path never reads video memory. `read_pixel`/`cell_shows` still inspect the actual framebuffer, so they see the previous image until a flush.

scroll_pages changes the viewport by pages with one row of overlap, preserving live cursor/colors. flush_row reads history or live cells. New output adjusts the offset to anchor the selected rows; if the oldest visible row expires, the view clamps to retained history. PageDown to the end, Escape or ordinary input calls return_live. COM1 remains a sequential stream; local navigation only changes the framebuffer.

CSI ?1049h/l saves/restores text, cursor, colors and bottom-margin state without allocation. Repeated entry preserves the saved screen. Alternate-screen scrolling is not archived and page navigation is disabled there. CSI 3J clears primary history without erasing live text; it preserves shell history on the alternate screen. CSI 2J erases cells without clearing history. Only scrolled-off rows are archived: arbitrary ANSI overwrites do not preserve prior frames.

Early constructors need no heap and draw directly. Enabling buffering clears early output; the screen banner follows `osel::init`, while COM1 announces it beforehand. Allocation failure retains direct rendering and reports to COM1. Panic rendering works in either mode and explicitly flushes.

The expensive code path identified in [issue #12](https://github.com/vszholobov/OSel/issues/12) was copying almost the entire framebuffer on every scroll with IRQs masked. At 2560×1600, four bytes per pixel and a 16×32 font, each scroll read roughly 15.3 MiB of video memory and wrote as much. Character-at-a-time cat also prevented batching; see [shell output](../shell.md#how). Actual video-memory cost and the final Lenovo speedup still require laptop measurements: QEMU does not reproduce those read costs.

## Code {#code}

`osel-core/src/console.rs`: Console, Color, ConsoleError, encode/decode, cell_shows/cell_shows_in. `osel-core/src/font.rs`: raster selection; `osel-core/fonts/console.rs`: tables. `src/drivers/avganec.rs`: WRITER, init, macros, try_print, with_console/read_pixel, panic_screen.

## Embedded font and license {#font}

OSel Console is a renamed 97-glyph subset of Terminus Font 4.49.1, respecting its reserved-name requirement. Bitmap shapes are unchanged; data is embedded without filesystem, heap or floating-point requirements. Pinned inputs are ter-u16n.bdf and ter-u32n.bdf. `tools/import-console-font.py` takes both and emits `osel-core/fonts/console.rs`; ordinary builds download nothing. Copyright/SIL OFL 1.1 remain in `osel-core/fonts/OFL.txt` and ramdisk /etc/font-license.txt.

## Framebuffer formats {#formats}

| Format | Bytes/pixel | Supported |
|---|---|---|
| RGB | 3 or 4 | Yes |
| BGR | 3 or 4 | Yes, standard QEMU UEFI |
| U8 grayscale | 1–4 | Yes, converted to brightness |
| Unknown, including BIOS 16-bit 5:6:5 | Any | No, console disabled |

## Invariants {#invariants}

Writes stay within stride×height×bytes_per_pixel and do not touch row padding. cell_shows compares glyph/current colors; cell_shows_in accepts explicit colors.

## Locking {#locks}

WRITER is IrqSpinlock; hardware IRQs do not print and exceptions use try_print. User output takes WRITER to update cells for one bounded chunk, then separately for each flush_row. The incoming IRQ state is restored between rows, allowing timer delivery and scheduling during normal scrolling/ANSI clearing. tty serializes writers with sleeping waits. print!, try_print, with_console and panic rendering flush under one lock; long user output must use the terminal.

## Pitfalls {#pitfalls}

Validate tiny/unknown framebuffers to avoid recursive rendering panic. Only Ж is included from Cyrillic; the source font itself has broader coverage. Normal scrolling must use RAM cells rather than copying framebuffer pixels. Direct scrolling remains only for early boot/allocation failure. Compiler fences after terminal/panic output preserve video-memory writes.

## Tests {#tests}

Host console tests compare buffered/direct pixels across formats/fonts/scales/ANSI, reconstruct scrolling from RAM after framebuffer corruption, skip unchanged cells, exercise history wrap/clamping/colors, anchored output and alternate-screen restoration, and cover geometry, wrapping, scrolling, tab/Backspace, formats, bounds, split ANSI, clearing/colors and Ж. Font tests cover selection, boundaries, replacement and native large raster versus scaling. `src/drivers/avganec.rs` checks RAM buffering and real framebuffer/pixel bounds/font selection; `tests/console_output.rs` exercises actual license cat, bounded writes, final text and 65 KiB of output with timer/scheduling/PS2 queue progress. Its SCROLL diagnostics report bytes, writes, ticks and input progress, not laptop timings; integration includes `tests/basic_boot.rs`, `tests/no_framebuffer.rs`, `tests/panic_screen.rs`.

Initial scroll-fix comparison before adding history: UEFI, QEMU 11.1.2, debug build, 128 MiB RAM, 1280×1024, baseline master `0a0b3a2` versus the working fix:

| Scenario | Before | After |
|---|---|---|
| License, 4547 bytes: write calls | 4547 | 9 |
| License: guest ticks | 153 | 193 |
| 65 KiB, 1024 lines: guest ticks | 2437 | 306 |

The timer runs at 100 Hz. These are diagnostic guest counters from one run, without a test speed threshold. Short license output did not improve in QEMU: its framebuffer copies are cheap while glyph rendering uses CPU. The laptop still needs before/after timings for `cat /etc/font-license.txt`, longer output, input during output and Tetris. Reproduce the integration test with `OSEL_BOOT=uefi cargo test --test console_output`, then `OSEL_BOOT=bios`.

## Decisions {#decisions}

[0001](../../decisions/0001-bootloader-0.11-and-uefi.md).

_Checked against code: 2026-10-15, fix-console-scroll branch._
