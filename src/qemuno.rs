//! qemuno module provides qemu specific methods

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[allow(dead_code)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Exits QEMU through the `isa-debug-exit` device. Without the device (a normal run
/// or real hardware) the machine just halts.
pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
