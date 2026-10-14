//! Foreground diagnostics; copy lock-protected state before console output.
use core::fmt::Write;

pub fn pci(out: &mut impl Write) {
    for device in crate::drivers::pci::scan() {
        let subsystem = device.read(0x2c);
        let _ = writeln!(
            out,
            "{:02x}:{:02x}.{} {:04x}:{:04x} rev={:02x} class={:02x}:{:02x}\n  subsystem={:04x}:{:04x} BAR0={:?} command={:04x}",
            device.bus,
            device.slot,
            device.function,
            device.vendor,
            device.device,
            device.revision,
            device.class,
            device.subclass,
            subsystem as u16,
            subsystem >> 16,
            device.bar0(),
            device.read_word(4)
        );
    }
}

pub fn report(out: &mut impl Write) {
    let heap = crate::allocator::with_allocator(|a| {
        (a.size(), a.in_use(), a.cached_bytes(), a.fallback_free())
    });
    let frames = crate::memory::used_frames();
    let total = crate::memory::total_usable_frames();
    let ticks = crate::interrupts::ticks();
    let _ = writeln!(
        out,
        "=== OSel diagnostics BEGIN ===\nOSel {} commit {}\nUptime {} s, PIT ticks={} rate={} Hz\nHeap size={} used={} cached={} free={}\nFrames used={frames}/{total} (4096 bytes each)",
        crate::VERSION,
        crate::COMMIT,
        crate::time::monotonic_ns() / 1_000_000_000,
        ticks,
        crate::interrupts::TICK_HZ,
        heap.0,
        heap.1,
        heap.2,
        heap.3
    );
    let _ = crate::keyboard::write_diagnostics(out);
    pci(out);
    crate::net::status(out);
    for section in ["ip", "route", "dns", "tcp"] {
        if let Err(error) = crate::net::diagnostics(section, out) {
            let _ = writeln!(out, "net {section}: {error}");
        }
    }
    let _ = crate::drivers::qca6174::command("status", out);
    let _ = crate::drivers::qca6174::command("fw", out);
    let _ = writeln!(out, "=== OSel diagnostics END ===");
}
