//! The stack a program starts with, as the System V x86_64 ABI lays it out
//!
//! From the top down: 16 random bytes for `AT_RANDOM`, the argument and environment
//! strings, padding, the auxiliary vector, `envp`, `argv` and `argc` at the stack pointer,
//! which is 16 byte aligned. Linux puts the strings above the random bytes; nothing
//! relies on the order.

use crate::errno::{Errno, KResult};
use crate::elf::PHENT_SIZE;
use crate::memory::PAGE_SIZE;
use alloc::vec;
use alloc::vec::Vec;

/// OSel's own limit for everything on the initial stack: Linux allows more, but a program
/// here gets a handful of arguments
pub const MAX_INITIAL_STACK_BYTES: usize = 128 * 1024;

pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_ENTRY: u64 = 9;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_SECURE: u64 = 23;
pub const AT_RANDOM: u64 = 25;

/// What the auxiliary vector tells about the loaded program
#[derive(Debug, Clone, Copy)]
pub struct AuxValues {
    pub phdr: Option<u64>,
    pub phnum: u16,
    pub entry: u64,
}

/// The bytes of the initial stack: they go to `rsp..stack_top`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackImage {
    pub rsp: u64,
    pub bytes: Vec<u8>,
}

/// Lays out the initial stack below `stack_top` (16 byte aligned). Strings must not
/// contain NUL (`EINVAL`), everything together must fit `MAX_INITIAL_STACK_BYTES` (`E2BIG`).
/// No `AT_SYSINFO_EHDR`: without a vDSO Go makes real `clock_gettime` system calls.
pub fn build(stack_top: u64, argv: &[&[u8]], envp: &[&[u8]], aux: &AuxValues, random: [u8; 16]) -> KResult<StackImage> {
    assert_eq!(stack_top % 16, 0, "stack top is not aligned");
    if argv.iter().chain(envp).any(|s| s.contains(&0)) {
        return Err(Errno::EINVAL);
    }
    let strings: usize = argv.iter().chain(envp).map(|s| s.len() + 1).sum();
    let random_at = stack_top.checked_sub(16).ok_or(Errno::E2BIG)?;
    let strings_at = random_at.checked_sub(strings as u64).ok_or(Errno::E2BIG)?;

    let mut auxv = Vec::new();
    if let Some(phdr) = aux.phdr {
        auxv.push((AT_PHDR, phdr));
    }
    auxv.extend([
        (AT_PHENT, PHENT_SIZE as u64),
        (AT_PHNUM, aux.phnum as u64),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_ENTRY, aux.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_RANDOM, random_at),
        (AT_NULL, 0),
    ]);
    // argc, argv and its NULL, envp and its NULL, the auxv pairs
    let words = 1 + argv.len() + 1 + envp.len() + 1 + 2 * auxv.len();
    let rsp = strings_at.checked_sub(words as u64 * 8).ok_or(Errno::E2BIG)? & !15;
    let size = (stack_top - rsp) as usize;
    if size > MAX_INITIAL_STACK_BYTES {
        return Err(Errno::E2BIG);
    }

    let mut bytes = vec![0u8; size];
    let at = |address: u64| (address - rsp) as usize;
    bytes[at(random_at)..].copy_from_slice(&random);
    let mut pointers = Vec::with_capacity(words);
    pointers.push(argv.len() as u64);
    let mut string = strings_at;
    for list in [argv, envp] {
        for s in list {
            bytes[at(string)..at(string) + s.len()].copy_from_slice(s);
            pointers.push(string);
            string += s.len() as u64 + 1;
        }
        pointers.push(0);
    }
    for (key, value) in auxv {
        pointers.extend([key, value]);
    }
    for (index, pointer) in pointers.iter().enumerate() {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&pointer.to_le_bytes());
    }
    Ok(StackImage { rsp, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: u64 = 0x7fff_ffff_f000;
    const RANDOM: [u8; 16] = *b"0123456789abcdef";
    const AUX: AuxValues = AuxValues { phdr: Some(0x40_0040), phnum: 6, entry: 0x47_fa60 };

    fn word(image: &StackImage, address: u64) -> u64 {
        let at = (address - image.rsp) as usize;
        u64::from_le_bytes(image.bytes[at..at + 8].try_into().unwrap())
    }

    fn string(image: &StackImage, address: u64) -> Vec<u8> {
        let at = (address - image.rsp) as usize;
        image.bytes[at..].iter().copied().take_while(|&b| b != 0).collect()
    }

    /// The whole image, written out by hand
    #[test]
    fn layout_byte_for_byte() {
        let image = build(TOP, &[b"/bin/hello", b"-v"], &[b"A=1"], &AUX, RANDOM).unwrap();
        // strings: "/bin/hello\0-v\0A=1\0" is 18 bytes below the 16 random ones
        let random_at = TOP - 16;
        let strings_at = random_at - 18;
        // argc + 2 argv + NULL + 1 envp + NULL + 12 auxv pairs = 30 words, then align down
        let rsp = (strings_at - 30 * 8) & !15;
        assert_eq!(image.rsp, rsp);
        assert_eq!(image.bytes.len() as u64, TOP - rsp);

        let mut expected = Vec::new();
        for w in [2, strings_at, strings_at + 11, 0, strings_at + 14, 0] {
            expected.extend_from_slice(&u64::to_le_bytes(w));
        }
        for (key, value) in [
            (AT_PHDR, 0x40_0040),
            (AT_PHENT, 56),
            (AT_PHNUM, 6),
            (AT_PAGESZ, 4096),
            (AT_ENTRY, 0x47_fa60),
            (AT_UID, 0),
            (AT_EUID, 0),
            (AT_GID, 0),
            (AT_EGID, 0),
            (AT_SECURE, 0),
            (AT_RANDOM, random_at),
            (AT_NULL, 0),
        ] {
            expected.extend_from_slice(&u64::to_le_bytes(key));
            expected.extend_from_slice(&u64::to_le_bytes(value));
        }
        expected.resize((strings_at - rsp) as usize, 0);
        expected.extend_from_slice(b"/bin/hello\0-v\0A=1\0");
        expected.extend_from_slice(&RANDOM);
        assert_eq!(image.bytes, expected);
    }

    #[test]
    fn the_stack_pointer_is_aligned_for_any_count() {
        for argc in 0..5 {
            for envc in 0..3 {
                for phdr in [None, Some(0x40_0040)] {
                    let args: Vec<&[u8]> = (0..argc).map(|_| &b"arg"[..]).collect();
                    let env: Vec<&[u8]> = (0..envc).map(|_| &b"K=V"[..]).collect();
                    let aux = AuxValues { phdr, ..AUX };
                    let image = build(TOP, &args, &env, &aux, RANDOM).unwrap();
                    assert_eq!(image.rsp % 16, 0, "{argc} {envc}");
                    assert_eq!(word(&image, image.rsp), argc as u64);
                    let envp = image.rsp + 8 * (argc as u64 + 2);
                    assert_eq!(word(&image, envp - 8), 0, "argv ends with NULL");
                    assert_eq!(word(&image, envp + 8 * envc as u64), 0, "envp ends with NULL");
                    for i in 0..envc as u64 {
                        assert_eq!(string(&image, word(&image, envp + 8 * i)), b"K=V");
                    }
                }
            }
        }
    }

    #[test]
    fn auxv_has_what_go_reads() {
        let image = build(TOP, &[b"go"], &[], &AUX, RANDOM).unwrap();
        let mut auxv = image.rsp + 8 * 4;
        let mut found = Vec::new();
        loop {
            let (key, value) = (word(&image, auxv), word(&image, auxv + 8));
            if key == AT_NULL {
                break;
            }
            found.push((key, value));
            auxv += 16;
        }
        let get = |key| found.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(get(AT_PAGESZ), Some(4096));
        assert_eq!(get(AT_SECURE), Some(0));
        let random = get(AT_RANDOM).unwrap();
        let at = (random - image.rsp) as usize;
        assert_eq!(image.bytes[at..at + 16], RANDOM);
        assert_eq!(get(33), None, "no AT_SYSINFO_EHDR: there is no vDSO");
    }

    #[test]
    fn bad_arguments() {
        assert_eq!(build(TOP, &[b"a\0b"], &[], &AUX, RANDOM), Err(Errno::EINVAL));
        assert_eq!(build(TOP, &[], &[b"K=\0"], &AUX, RANDOM), Err(Errno::EINVAL));
        let big = vec![b'x'; MAX_INITIAL_STACK_BYTES];
        assert_eq!(build(TOP, &[&big], &[], &AUX, RANDOM), Err(Errno::E2BIG));
        let many: Vec<&[u8]> = (0..MAX_INITIAL_STACK_BYTES / 8).map(|_| &b""[..]).collect();
        assert_eq!(build(TOP, &many, &[], &AUX, RANDOM), Err(Errno::E2BIG));
        assert_eq!(build(64, &[b"low"], &[], &AUX, RANDOM), Err(Errno::E2BIG), "no room below the top");
        assert!(build(TOP, &[&big[..MAX_INITIAL_STACK_BYTES / 2]], &[], &AUX, RANDOM).is_ok());
    }
}
