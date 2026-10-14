#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
use alloc::string::String;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;

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

#[test_case]
fn builtins_and_errors_use_the_real_vfs() {
    let mut out = String::new();
    assert_eq!(osel::shell::execute("help", &mut out), 0);
    assert!(out.contains("tetris"));
    out.clear();
    assert_eq!(osel::shell::execute("kbd", &mut out), 0);
    assert!(out.contains("PS/2 init: Ready, error=None"));
    assert!(out.contains("KBD t=") && out.contains("irq="));
    assert_ne!(osel::shell::execute("kbd extra", &mut out), 0);
    out.clear();
    assert_eq!(osel::shell::execute("ls /bin", &mut out), 0);
    assert!(out.contains("hello\n") && out.contains("clear\n"));
    out.clear();
    assert_eq!(osel::shell::execute("cat /etc/motd", &mut out), 0);
    assert!(out.contains("OSel"));
    for line in [
        "missing-program",
        "cat /missing",
        "ls /etc/motd",
        "cat /dev/tty",
        "cat",
        "/etc/motd",
    ] {
        assert_ne!(osel::shell::execute(line, &mut out), 0, "{line}");
    }
    assert_eq!(osel::shell::execute("hello | clear", &mut out), 2);
    assert_eq!(osel::shell::execute("  ", &mut out), 0);
}

#[test_case]
fn diagnostics_and_missing_wifi_are_actionable() {
    let mut out = String::new();
    assert_eq!(osel::shell::execute("diag", &mut out), 0);
    assert!(out.contains("diagnostics BEGIN") && out.contains("diagnostics END"));
    assert!(out.contains(osel::COMMIT));
    assert!(out.contains("QCA6174") && out.contains("board=8124"));
    assert_eq!(osel::shell::execute("wifi probe", &mut out), 1);
    assert!(out.contains("ENODEV"));
    for command in ["wifi invalid", "wifi scan", "ping", "ping 10.0.2.2 0", "ping 10.0.2.2 abc", "nslookup", "tcpcheck", "tcpcheck 10.0.2.2 0", "tcpcheck 10.0.2.2 65536", "net invalid", "diag extra", "pci extra"] {
        assert_ne!(osel::shell::execute(command, &mut out), 0, "{command}");
    }
    assert_eq!(osel::shell::execute("cat /usr/share/licenses/ath10k/LICENSE", &mut out), 0);
    assert!(out.contains("Qualcomm Atheros"));
}

#[test_case]
fn repeated_programs_return_to_the_shell_without_leaks() {
    let settings = osel::tty::get_termios();
    let mut out = String::new();
    for _ in 0..3 {
        assert_eq!(osel::shell::execute("hello", &mut out), 0);
        assert_eq!(osel::shell::execute("/bin/clear", &mut out), 0);
        assert_eq!(osel::tty::get_termios(), settings);
    }
    if osel::fs::lookup("/bin/gohello").is_ok() {
        assert_eq!(osel::shell::execute("gohello", &mut out), 30);
        assert!(out.contains("exit 30"));
    }
    assert_eq!(osel::shell::execute("ls /", &mut out), 0);
}
