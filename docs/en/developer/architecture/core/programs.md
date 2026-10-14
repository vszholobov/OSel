---
description: Static ELF loading, program address spaces and the main stack with argv, envp and auxv.
---

# Programs

Language: [Русский](../../../../ru/developer/architecture/core/programs.md) · [English](programs.md)

**Audience:** developers changing loading or initial process state.

**Summary:** a program is a static x86_64 ELF64 from the filesystem, as built by Go with `CGO_ENABLED=0`. `program::load` creates `UserSpace`; the core loader maps zeroed pages and copies segments. The stack follows System V ABI. See [user mode](user-mode.md) for execution.

## Behavior {#how}

### Parse ELF {#elf}

`elf::parse` in `osel-core/src/elf.rs` rejects unsupported input with ENOEXEC. It requires little-endian ELF64, ET_EXEC, EM_X86_64, 1–64 program headers of 56 bytes inside the file. PT_INTERP/PT_DYNAMIC are rejected; PT_TLS, PT_PHDR, PT_NOTE, PT_GNU_STACK and others are not used (Go sets TLS through arch_prctl).

Nonempty PT_LOAD segments require filesz ≤ memsz, in-file data, no address overflow, sorted nonoverlapping ranges from MIN_USER_ADDR (64 KB) to USER_END, and no combined write/execute permission. Entry must lie in executable memory. AT_PHDR points to the program headers if a loaded segment contains them. `ReadAt` accepts byte slices or `dyn Inode`, retries short reads and rejects early EOF.

### Load {#load}

`elf::load` computes `page_plan`: a shared page combines segment permissions, rejecting writable code. `ProgramMemory` maps zeroed pages and copies file bytes page by page; .bss remains zero. Segments must fit the supplied boundary and MAX_PROGRAM_BYTES (256 MB), otherwise ENOMEM before mapping. Failure may leave a partial image; dropping the whole space rolls it back.

`UserSpace` implements ProgramMemory in `src/program.rs`: map_zeroed calls map; write uses write_bytes through the physical-memory mapping, independently of user page permissions.

### Main-thread stack {#stack}

`program::load_file` maps writable, nonexecutable STACK_SIZE (256 KB) below STACK_TOP. The final lower-half page and the guard below the stack stay unmapped; segments end below the guard. Stack and segments become [areas](memory.md#areas), but the guard does not.

### Initial stack {#initial-stack}

`initial_stack::build` in `osel-core/src/initial_stack.rs` lays out 16 AT_RANDOM bytes, NUL-terminated argv/envp strings, 16-byte alignment, auxv, envp pointers/NULL, argv pointers/NULL and argc at RSP.

Auxv includes AT_PHDR when known, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_ENTRY, AT_UID, AT_EUID, AT_GID, AT_EGID, AT_SECURE, AT_RANDOM and AT_NULL. No AT_SYSINFO_EHDR means no vDSO: Go calls clock_gettime. Go needs AT_RANDOM, AT_PAGESZ and AT_SECURE; other entries serve libc. Missing page size/random would send Go to /proc/self/auxv or /dev/urandom. Embedded NUL returns EINVAL; exceeding MAX_INITIAL_STACK_BYTES (128 KB) returns E2BIG. Tick-seeded splitmix64 supplies bytes for Go hash seeding, not cryptography.

## Go programs {#go}

`user-go/gohello/main.go` prints hello from go, runs five goroutines with time.Sleep, sums squared indices and exits 30. Runner packages /bin/gohello; see [build settings](../../reference/runner.md#go-programs).

`tests/go_hello.rs` calls program::run with `GODEBUG=asyncpreemptoff=1`, requires Stop::Exit(30) and checks heap/frame recovery. ENOENT skips an absent binary; CI installs Go and tests both modes. Normal boot runs /bin/hello, then the shell rather than automatically running Go.

Async Go preemption is disabled because signals are not delivered. Nil access ends the process instead of raising a Go panic. This does not disable [kernel thread preemption](threads.md#preemption).

## Code {#code}

`osel-core/src/elf.rs`: parse, page_plan, load, ReadAt, ProgramMemory, ElfBuilder. `osel-core/src/initial_stack.rs`: build, AuxValues, AT_* constants. `src/program.rs`: load/load_file, STACK_TOP/STACK_SIZE and UserSpace integration. `src/memory.rs`: write_bytes, read_bytes, page_flags.

## Invariants {#invariants}

Fresh pages expose no prior contents. No mappings below MIN_USER_ADDR, at the last lower-half page or stack guard. No writable executable pages.

## Locking and interrupt context {#locks}

Loading allocates heap memory and takes MEMORY in UserSpace operations. Run it in ordinary kernel code, never IRQ handlers.

## Pitfalls {#pitfalls}

cpio bytes are not page-aligned, so copy rather than directly map them. Linux permits shared code/data pages and requires matching address/file-offset page alignment; OSel rejects writable code pages but does not require matching offsets because it copies. Go's separately aligned segments are compatible. Drop partial spaces on error instead of undoing individual segment writes.

## Tests {#tests}

ELF host tests check fake-memory writes, AT_PHDR, bad headers/segments, ignored Go headers, shared-page permissions, neighboring segments, short reads and 5000 random corruptions without panic. Stack tests check byte layout, all argument counts/alignment, auxv and EINVAL/E2BIG. `src/program.rs` checks real rights, .bss, stack/guards, malformed files, failure at each allocation with ENOMEM and no leaks, and the segment boundary.

## Decisions {#decisions}

[0005](../../decisions/0005-linux-abi-for-go.md), [0010](../../decisions/0010-static-elf-loader.md).

_Checked against code: 2026-10-13, commit 9c0f1c6._
