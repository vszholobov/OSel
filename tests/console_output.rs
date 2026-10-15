#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Real cat/TTY scrolling, timing diagnostics and scheduling/input during output.

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use osel_core::file::FileLike;
use osel_core::vfs::Stat;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

struct Terminal {
    tty: osel::tty::Tty,
    writes: usize,
}

impl Terminal {
    fn new() -> Self {
        Self { tty: osel::tty::Tty::new(Stat::default()), writes: 0 }
    }
}

impl Write for Terminal {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        assert!(x86_64::instructions::interrupts::are_enabled());
        self.writes += 1;
        self.tty.write(0, text.as_bytes()).map(|_| ()).map_err(|_| fmt::Error)
    }
}

#[test_case]
fn cat_font_license_scrolls_through_the_real_terminal() {
    let mut terminal = Terminal::new();
    terminal.write_str("\x1b[0m\x1b[H\x1b[2J").unwrap();
    terminal.writes = 0;
    let start = osel::interrupts::ticks();
    assert_eq!(osel::shell::execute("cat /etc/font-license.txt", &mut terminal), 0);
    let elapsed = osel::interrupts::ticks() - start;
    let bytes = osel::fs::read_file("/etc/font-license.txt").unwrap().len();
    osel::serial_println!("\nSCROLL cat: bytes={} writes={} ticks={}", bytes, terminal.writes, elapsed);
    assert!(terminal.writes <= bytes / 500 + 2, "cat must batch output");
    osel::avganec::with_console(|c| {
        let last = "OTHER DEALINGS IN THE FONT SOFTWARE.";
        let (row, column) = c.cursor();
        assert_eq!((row, column), (c.rows() - 1, 0));
        assert!(last.chars().enumerate().all(|(col, ch)| c.cell_shows(row - 1, col, ch)));
    }).expect("no framebuffer");
}

static DONE: AtomicBool = AtomicBool::new(false);
static INPUT_PROGRESS: AtomicUsize = AtomicUsize::new(0);

fn input_probe() {
    osel::time::sleep_until(osel::time::monotonic_ns() + 20_000_000).unwrap();
    while !DONE.load(Ordering::SeqCst) {
        // Exercise the same decoder/queue as PS/2 while the writer is active.
        x86_64::instructions::interrupts::without_interrupts(|| {
            osel::keyboard::handle_scancode(0x1e);
            osel::keyboard::handle_scancode(0x9e);
        });
        assert_eq!(osel::keyboard::pop_char(), Some('a'));
        INPUT_PROGRESS.fetch_add(1, Ordering::SeqCst);
        osel::time::sleep_until(osel::time::monotonic_ns() + 10_000_000).unwrap();
    }
}

#[test_case]
fn long_output_keeps_timer_scheduler_and_keyboard_queue_running() {
    let mut text = alloc::string::String::new();
    for line in 0..1024 {
        writeln!(text, "line {line:04}: ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz").unwrap();
    }
    DONE.store(false, Ordering::SeqCst);
    INPUT_PROGRESS.store(0, Ordering::SeqCst);
    let probe = osel::scheduler::spawn_kernel(input_probe).unwrap();
    let start = osel::interrupts::ticks();
    let mut terminal = Terminal::new();
    // Each call goes through tty's own bounded batching, UTF-8 and output guard.
    terminal.write_str(&text).unwrap();
    let elapsed = osel::interrupts::ticks() - start;
    DONE.store(true, Ordering::SeqCst);
    while osel::scheduler::runs(probe).is_some() {
        assert!(osel::interrupts::ticks() - start < elapsed + 100);
        osel::scheduler::yield_now();
    }
    let progress = INPUT_PROGRESS.load(Ordering::SeqCst);
    assert!(elapsed > 0 && progress > 0, "timer/input worker must run during output");
    osel::serial_println!("\nSCROLL long: bytes={} ticks={} input_progress={}", text.len(), elapsed, progress);
}
