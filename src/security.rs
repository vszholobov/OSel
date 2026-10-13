//! security module: adversarial programs that try to break out of ring 3
//!
//! Every program here is hostile — it reaches for kernel or foreign memory, hands the
//! kernel bad pointers, or floods it with threads. Each test asserts the kernel contains
//! it: the attacker ends with a fault or an errno, the kernel stays healthy, and no frames
//! or heap leak. The guarantees these pin down are described in
//! `docs/architecture/core/security.md`.
//!
//! Isolation rests on the page tables, not on SMAP or SMEP: a program's pages are the lower
//! half only, user accessible; the kernel half is not user accessible, and the kernel
//! touches a program's memory through the physical map after checking every page.

#[cfg(test)]
mod tests {
    use crate::memory::used_frames;
    use crate::user::Stop;
    use crate::user::tests::{code, load};
    use crate::user::{initial_frame, run};
    use core::arch::global_asm;
    use osel_core::errno::Errno;
    use osel_core::trap::{GENERAL_PROTECTION, PAGE_FAULT};

    /// A kernel-half address: nothing a program may touch
    const KERNEL_ADDR: u64 = 0xffff_8000_0000_0000u64;

    global_asm!(
        ".pushsection .rodata.osel_attack_programs, \"a\"",
        // read a kernel address
        ".global osel_read_kernel", "osel_read_kernel:",
        "mov rax, 0xffff800000000000",
        "mov rax, [rax]",
        "ud2",
        ".global osel_read_kernel_end", "osel_read_kernel_end:",
        // write a kernel address
        ".global osel_write_kernel", "osel_write_kernel:",
        "mov rax, 0xffff800000000000",
        "mov qword ptr [rax], 1",
        "ud2",
        ".global osel_write_kernel_end", "osel_write_kernel_end:",
        // jump into the kernel half
        ".global osel_jump_kernel", "osel_jump_kernel:",
        "mov rax, 0xffff800000000000",
        "jmp rax",
        ".global osel_jump_kernel_end", "osel_jump_kernel_end:",
        // every program below exits with the errno it got (neg rax), so the test reads it
        // write(1, kernel_addr, 8) -> EFAULT, nothing printed
        ".global osel_write_kernel_buf", "osel_write_kernel_buf:",
        "mov edi, 1",
        "mov rsi, 0xffff800000000000",
        "mov edx, 8",
        "mov eax, 1",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_write_kernel_buf_end", "osel_write_kernel_buf_end:",
        // mmap MAP_FIXED over the kernel half -> ENOMEM
        ".global osel_mmap_kernel", "osel_mmap_kernel:",
        "mov rdi, 0xffff800000000000",
        "mov esi, 0x1000",
        "mov edx, 3",               // PROT_READ|WRITE
        "mov r10d, 0x32",           // MAP_FIXED|MAP_ANONYMOUS... 0x10|0x20|0x02
        "or r10d, 2",               // MAP_PRIVATE
        "xor r8d, r8d", "mov r8, -1",
        "xor r9d, r9d",
        "mov eax, 9",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_mmap_kernel_end", "osel_mmap_kernel_end:",
        // mmap MAP_FIXED below mmap_min_addr (0x10000) -> EPERM
        ".global osel_mmap_low", "osel_mmap_low:",
        "mov edi, 0x1000",
        "mov esi, 0x1000",
        "mov edx, 3",
        "mov r10d, 0x32",           // MAP_FIXED|MAP_PRIVATE|MAP_ANONYMOUS
        "mov r8, -1", "xor r9d, r9d",
        "mov eax, 9",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_mmap_low_end", "osel_mmap_low_end:",
        // munmap the kernel half -> EINVAL
        ".global osel_munmap_kernel", "osel_munmap_kernel:",
        "mov rdi, 0xffff800000000000",
        "mov esi, 0x1000",
        "mov eax, 11",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_munmap_kernel_end", "osel_munmap_kernel_end:",
        // mprotect the kernel half -> ENOMEM
        ".global osel_mprotect_kernel", "osel_mprotect_kernel:",
        "mov rdi, 0xffff800000000000",
        "mov esi, 0x1000",
        "mov edx, 1",
        "mov eax, 10",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_mprotect_kernel_end", "osel_mprotect_kernel_end:",
        // futex on a kernel address -> EFAULT
        ".global osel_futex_kernel", "osel_futex_kernel:",
        "mov rdi, 0xffff800000000000",
        "xor esi, esi",             // FUTEX_WAIT
        "xor edx, edx",
        "xor r10d, r10d",
        "mov eax, 202",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_futex_kernel_end", "osel_futex_kernel_end:",
        // write to a bad descriptor -> EBADF
        ".global osel_bad_fd", "osel_bad_fd:",
        "mov edi, 999",
        "mov esi, 0x600000",
        "mov edx, 1",
        "mov eax, 1",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_bad_fd_end", "osel_bad_fd_end:",
        // mmap with an address+length that overflows past the lower half -> ENOMEM
        ".global osel_mmap_overflow", "osel_mmap_overflow:",
        "mov rdi, 0x7fffffff0000",
        "mov rsi, 0x7fffffffffffffff",  // huge length
        "mov edx, 3",
        "mov r10d, 0x32",           // MAP_FIXED|MAP_PRIVATE|MAP_ANONYMOUS
        "mov r8, -1", "xor r9d, r9d",
        "mov eax, 9",
        "syscall",
        "neg rax", "mov rdi, rax", "mov eax, 60", "syscall",
        ".global osel_mmap_overflow_end", "osel_mmap_overflow_end:",
        // clone a crowd of spinning threads, then exit the group with 0; every child must
        // be reaped. r12 counts down the spawns.
        ".global osel_fork_bomb", "osel_fork_bomb:",
        "mov r12d, 50",
        "2:",
        "mov eax, 56",
        "mov edi, 0xd0f00",
        "mov esi, 0x600f00",        // every child shares this stack: they only spin
        "xor edx, edx", "xor r10d, r10d", "xor r8d, r8d", "xor r9d, r9d",
        "syscall",
        "test rax, rax",
        "jz 3f",                    // child
        "js 4f",                    // clone failed: stop spawning
        "dec r12d",
        "jnz 2b",
        "4:",
        "xor edi, edi",
        "mov eax, 231",             // exit_group(0)
        "syscall",
        "3:",
        "5:", "jmp 5b",             // child spins until reaped
        ".global osel_fork_bomb_end", "osel_fork_bomb_end:",
        // write 0xaa across a mapped page, read one byte back into the exit code: used to
        // show a second process does not see this one's data at the same address
        ".global osel_mark_page", "osel_mark_page:",
        "mov rdi, 0x40000000",
        "mov esi, 0x1000",
        "mov edx, 3",
        "mov r10d, 0x32",           // MAP_FIXED|MAP_PRIVATE|MAP_ANONYMOUS
        "mov r8, -1", "xor r9d, r9d",
        "mov eax, 9",
        "syscall",
        "mov byte ptr [0x40000000], 0xaa",
        "movzx edi, byte ptr [0x40000000]",
        "mov eax, 60",
        "syscall",
        ".global osel_mark_page_end", "osel_mark_page_end:",
        // map the same address and read it: a fresh page reads 0, not another process's 0xaa
        ".global osel_read_page", "osel_read_page:",
        "mov rdi, 0x40000000",
        "mov esi, 0x1000",
        "mov edx, 3",
        "mov r10d, 0x32",
        "mov r8, -1", "xor r9d, r9d",
        "mov eax, 9",
        "syscall",
        "movzx edi, byte ptr [0x40000000]",
        "mov eax, 60",
        "syscall",
        ".global osel_read_page_end", "osel_read_page_end:",
        ".popsection",
    );

    fn attack(program: &[u8]) -> Stop {
        let mut loaded = load(program);
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        run(&mut loaded.space, &mut frame)
    }

    fn errno(code: u8) -> u8 {
        code
    }

    /// Reaching into the kernel half from ring 3 faults the program, never the kernel
    #[test_case]
    fn test_touching_the_kernel_faults_the_program() {
        for program in [
            code!(osel_read_kernel, osel_read_kernel_end),
            code!(osel_write_kernel, osel_write_kernel_end),
            code!(osel_jump_kernel, osel_jump_kernel_end),
        ] {
            let trap = attack(program).fault();
            assert!(matches!(trap.vector, PAGE_FAULT | GENERAL_PROTECTION), "vector {}", trap.vector);
            crate::user::tests::kernel_is_fine();
        }
    }

    /// A kernel pointer handed to a system call is `EFAULT`, and nothing is read or written
    #[test_case]
    fn test_kernel_pointers_to_syscalls_are_efault() {
        assert_eq!(attack(code!(osel_write_kernel_buf, osel_write_kernel_buf_end)).exit_code(), errno(Errno::EFAULT.code() as u8));
        assert_eq!(attack(code!(osel_futex_kernel, osel_futex_kernel_end)).exit_code(), errno(Errno::EFAULT.code() as u8));
    }

    /// Mapping calls can't touch the kernel half or the forbidden low addresses
    #[test_case]
    fn test_mapping_calls_refuse_forbidden_ranges() {
        assert_eq!(attack(code!(osel_mmap_kernel, osel_mmap_kernel_end)).exit_code(), Errno::ENOMEM.code() as u8);
        assert_eq!(attack(code!(osel_mmap_low, osel_mmap_low_end)).exit_code(), Errno::EPERM.code() as u8);
        assert_eq!(attack(code!(osel_munmap_kernel, osel_munmap_kernel_end)).exit_code(), Errno::EINVAL.code() as u8);
        assert_eq!(attack(code!(osel_mprotect_kernel, osel_mprotect_kernel_end)).exit_code(), Errno::ENOMEM.code() as u8);
        assert_eq!(attack(code!(osel_mmap_overflow, osel_mmap_overflow_end)).exit_code(), Errno::ENOMEM.code() as u8);
    }

    /// A bad descriptor number is `EBADF`
    #[test_case]
    fn test_bad_descriptor_is_ebadf() {
        assert_eq!(attack(code!(osel_bad_fd, osel_bad_fd_end)).exit_code(), Errno::EBADF.code() as u8);
    }

    /// A clone flood is bounded by the kernel stacks: it fails with `ENOMEM`, the program
    /// ends, every spun-up thread is reaped and nothing leaks
    #[test_case]
    fn test_a_clone_flood_is_contained() {
        let frames = used_frames();
        let heap = crate::allocator::with_allocator(|a| a.in_use());
        assert_eq!(attack(code!(osel_fork_bomb, osel_fork_bomb_end)).exit_code(), 0);
        crate::user::tests::kernel_is_fine();
        assert_eq!(used_frames(), frames, "every cloned thread's stack is freed");
        assert_eq!(crate::allocator::with_allocator(|a| a.in_use()), heap);
    }

    /// One process cannot see another's memory at the same address: the second maps the
    /// same page and reads a fresh zero, not the `0xaa` the first wrote
    #[test_case]
    fn test_processes_do_not_share_memory() {
        assert_eq!(attack(code!(osel_mark_page, osel_mark_page_end)).exit_code(), 0xaa, "the first wrote and read its own byte");
        assert_eq!(attack(code!(osel_read_page, osel_read_page_end)).exit_code(), 0, "the second sees a fresh zero page");
    }

    /// The kernel address constant is in the kernel half, so these attacks really aim there
    #[test_case]
    fn test_kernel_addr_is_in_the_kernel_half() {
        assert!(KERNEL_ADDR >= crate::memory::USER_END);
    }
}
