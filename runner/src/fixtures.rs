//! Executables included only in the adversarial child-process test image.

use osel_core::elf::{ElfBuilder, PF_R, PF_W, PF_X};

pub fn child_programs() -> Vec<(String, Vec<u8>)> {
    // read(3, 0x600000, 1); exit(37) on one byte, exit(99) otherwise.
    // Waiting on fd 3 gives the parent a deterministic live child after exec.
    let code = [
        0x31, 0xc0, // xor eax,eax
        0xbf, 3, 0, 0, 0, // mov edi,3
        0xbe, 0, 0, 0x60, 0, // mov esi,0x600000
        0xba, 1, 0, 0, 0, // mov edx,1
        0x0f, 0x05, // syscall
        0xbf, 99, 0, 0, 0, // mov edi,99
        0x48, 0x83, 0xf8, 1, // cmp rax,1
        0x75, 5, // jne exit
        0xbf, 37, 0, 0, 0, // mov edi,37
        0xb8, 60, 0, 0, 0, // exit: mov eax,60
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
    ];
    let image = ElfBuilder::new(0x400000)
        .segment(0x400000, PF_R | PF_X, &code, code.len() as u64)
        .segment(0x600000, PF_R | PF_W, &[], 4096)
        .build();
    vec![
        ("pipe-child37".to_string(), image),
        ("bad-exec".to_string(), b"not an ELF".to_vec()),
    ]
}
