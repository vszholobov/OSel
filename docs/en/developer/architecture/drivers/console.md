---
description: Framebuffer console, built-in raster fonts, ANSI rendering and panic screen.
---

# Console

Language: [Русский](../../../../ru/developer/architecture/drivers/console.md) · [English](console.md)

**Audience:** developers changing display output.

**Summary:** OSel Console embeds Terminus-derived 16×32 and fallback 8×16 rasters. Large letters use a native raster rather than scaling. UEFI/BIOS share host-tested rendering in osel-core; kernel avganec owns globals, print macros and panic display.

## Behavior {#how}

Console in `osel-core/src/console.rs` uses a byte buffer and FrameBufferInfo. Cells are 8×16/16×32 with built-in line spacing. Text scrolls at the bottom. Newline advances a line, carriage return starts it, tab advances to a multiple-of-eight position without wrapping. Backspace erases the previous cell but does nothing at line start. Other controls are not drawn. Font coverage is printable ASCII, Ж and replacement �; unsupported characters use UNKNOWN_CHAR. Glyph pixels have sharp foreground/background colors without antialiasing.

new_with_font selects raster; new uses the small font. new_scaled supports integer ×1/×2/×3 of the small raster but kernel does not use it. Cell geometry, ANSI clearing, scrolling and TIOCGWINSZ all account for font/scale; COM1 output is unaffected.

avganec::init selects 16×32 when at least 80×25 fits, otherwise 8×16. The large raster requires at least 1280×800. QEMU's 1280×1024 gives 80×32. UEFI retains firmware selection because bootloader's minimum chooses the last suitable mode (2560×1600 in QEMU); BIOS requests 1280×800. Lookup occurs once per glyph; spaces fill cells directly; all glyph cells overwrite spacing. Clearing combines horizontal spans and fills uniform colors efficiently.

Console::new rejects unknown formats, bad pixel size/stride, a screen too small for one cell or a short buffer with ConsoleError.

ANSI parsing survives writes: CSI A/B/C/D, H/f, G move the cursor; J/K erase; coordinates clamp to screen. SGR supports basic/bright colors and reset; other attributes are consumed without rendering. ?25h/l records visibility but no text cursor is drawn yet. At most eight 16-bit parameters are allowed; overflow saturates and excess parameters invalidate a sequence. External clear emits ESC[H ESC[2J; console code does not launch programs.

Kernel init consumes framebuffer and reports failure to serial; screen printing before init/without framebuffer is a no-op. try_print avoids waiting for exception output. with_console/read_pixel support inspection; panic_screen draws failures.

## Code {#code}

`osel-core/src/console.rs`: Console, Color, ConsoleError, encode/decode, cell_shows/cell_shows_in. `osel-core/src/font.rs`: raster selection; `osel-core/fonts/console.rs`: tables. `src/avganec.rs`: WRITER, init, macros, try_print, with_console/read_pixel, panic_screen.

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

WRITER is IrqSpinlock; hardware IRQs do not print and exceptions use try_print. User output takes WRITER per character so IRQs/switches can occur between them. tty separately serializes writers with sleeping waits. One ANSI clear/scroll still runs atomically under WRITER; kernel diagnostics print entire messages.

## Pitfalls {#pitfalls}

Validate tiny/unknown framebuffers to avoid recursive rendering panic. Only Ж is included from Cyrillic; the source font itself has broader coverage. Scrolling copy_within reads video memory and may be slow on hardware; see [debt](../../debt.md). A compiler fence after panic rendering preserves writes because kernel code does not otherwise read framebuffer contents.

## Tests {#tests}

Host console tests cover geometry, wrapping, scrolling, tab/Backspace, formats, bounds, split ANSI, clearing/colors and Ж. Font tests cover selection, boundaries, replacement and native large raster versus scaling. `src/avganec.rs` checks real framebuffer/pixel bounds/font selection; integration includes `tests/basic_boot.rs`, `tests/no_framebuffer.rs`, `tests/panic_screen.rs`.

## Decisions {#decisions}

[0001](../../decisions/0001-bootloader-0.11-and-uefi.md).

_Checked against code: 2026-10-13, working branch for stage 3._
