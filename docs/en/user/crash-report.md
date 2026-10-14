---
description: What the red OSel crash screen means and how to report a failure.
---

# Crash reports

**Audience:** anyone seeing a red `OSel crashed :(` screen.

**Summary:** the kernel encountered an error and halted. Photograph the whole screen legibly and send the developer the image, computer model, boot image and preceding actions. Power off by holding the power button for 5–10 seconds.

## What to send {#report}

1. A legible photograph of the whole screen, including the version and commit on its second line.
2. Manufacturer and computer model, such as Samsung NP530U4B.
3. Image name: `OSel.uefi.img` or `OSel.bios.img`.
4. What you were doing: initial boot, pressing a key, or another action.

Attach the photo to a [GitHub issue](https://github.com/vszholobov/OSel/issues/new). This requires a GitHub account.

## Screen contents {#anatomy}

```text
OSel crashed :(
OSel 0.1.0, commit 2b7a890

panicked at src/interrupts.rs:…:
EXCEPTION: PAGE FAULT
  rip: 0xffff800000018c11  rsp: 0xffff808000014f80
  cs: 0x0008  ss: 0x0010  rflags: 0x10286
  error code: 0x0
  accessed address: 0xdeadbeef000
  reason: page not present on read by kernel
```

The first two lines identify the failure and build. The rest tells developers where it happened and the processor state.

{% cut "Technical fields" %}

| Field | Meaning |
|---|---|
| `panicked at …` | Source file and line where the kernel halted |
| `EXCEPTION: …` | Processor exception; absent when kernel code detects the error itself |
| `rip` | Address of the faulting instruction |
| `rsp` | Stack pointer at the fault |
| `cs`, `ss` | Code and stack segments; normal kernel values are `0x0008` and `0x0010` |
| `rflags` | Processor flags |
| `error code` | Processor error code; segment faults identify the relevant table entry |
| `accessed address` | Address accessed on a page fault |
| `reason` | Missing page or permission violation, access type and kernel/user origin |

Developer analysis is covered by [debugging](../developer/guides/debugging.md#panic-screen).

{% endcut %}

## Black screen or repeated reboot {#no-screen}

The kernel may have failed before displaying a report. Describe the visible output, seconds before reboot, and whether a line ending in `woke up!` appears. Send this information to the developer too.
