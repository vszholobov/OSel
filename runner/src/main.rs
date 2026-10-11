//! Cargo runner for the kernel: packs the kernel ELF into a bootable disk image
//! and boots it in QEMU. Replaces `bootimage runner` from bootloader 0.9 times.
//!
//! Usage: osel-runner <kernel-elf> [extra qemu args...]
//! Env:   OSEL_BOOT=uefi|bios (default: uefi)

use bootloader::{BootConfig, DiskImageBuilder};
use bootloader_boot_config::LevelFilter;
use ovmf_prebuilt::{Arch, FileType, Prebuilt, Source};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

/// `exit_qemu(QemuExitCode::Success)` makes QEMU exit with `(0x10 << 1) | 1`
const TEST_SUCCESS_EXIT_CODE: i32 = 33;
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boot {
    Uefi,
    Bios,
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let kernel = PathBuf::from(args.next().expect("usage: osel-runner <kernel-elf> [qemu args...]"));
    let mut extra_qemu_args: Vec<_> = args.collect();

    let boot = match parse_boot(env::var("OSEL_BOOT").ok().as_deref()) {
        Ok(boot) => boot,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let is_test = is_test_binary(&kernel);
    if is_test && !extra_qemu_args.is_empty() {
        // `cargo test -- <args>` would otherwise hand test filters to QEMU
        eprintln!("warning: test arguments are not supported and ignored: {extra_qemu_args:?}");
        extra_qemu_args.clear();
    }

    let image = create_disk_image(&kernel, boot);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.arg("-drive").arg(format!("format=raw,file={}", image.display()));
    if boot == Boot::Uefi {
        let ovmf = ovmf_firmware();
        qemu.arg("-drive").arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf.get_file(Arch::X64, FileType::Code).display()
        ));
        // snapshot=on keeps the shared vars file untouched between runs
        qemu.arg("-drive").arg(format!(
            "if=pflash,format=raw,snapshot=on,file={}",
            ovmf.get_file(Arch::X64, FileType::Vars).display()
        ));
    }
    qemu.args(["-serial", "stdio", "-no-reboot"]);
    if is_test {
        qemu.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-display", "none"]);
        // a test never reads the keyboard of the host, and QEMU must not switch the terminal to raw mode
        qemu.stdin(Stdio::null());
    }
    qemu.args(&extra_qemu_args);
    qemu.stdout(Stdio::piped());

    let mut child = qemu.spawn().expect("failed to launch qemu-system-x86_64");
    let output = forward_serial_output(&mut child);
    if is_test {
        let status = wait_test(&mut child);
        let summary = output.join().unwrap_or_default();
        match test_outcome(status, &summary) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("test failed: {message}");
                ExitCode::FAILURE
            }
        }
    } else {
        let status = child.wait().expect("failed to wait for qemu");
        let _ = output.join();
        ExitCode::from(run_exit_code(status))
    }
}

fn parse_boot(value: Option<&str>) -> Result<Boot, String> {
    match value {
        None | Some("") | Some("uefi") => Ok(Boot::Uefi),
        Some("bios") => Ok(Boot::Bios),
        Some(other) => Err(format!("unknown OSEL_BOOT value `{other}`, expected `uefi` or `bios`")),
    }
}

/// Test binaries are named `<target>-<16 hex digits hash>`, the kernel binary itself is
/// plain `OSel`. The directory can't be used: depending on the cargo version tests land
/// either in `target/<triple>/<profile>/deps` or in `.../build/<pkg>/<hash>/out`.
fn is_test_binary(kernel: &Path) -> bool {
    let name = kernel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    name.rsplit_once('-')
        .is_some_and(|(_, hash)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn create_disk_image(kernel: &Path, boot: Boot) -> PathBuf {
    let mut boot_config = BootConfig::default();
    // the bootloader traces every boot step by default, keep only problems
    boot_config.log_level = LevelFilter::Warn;

    let mut builder = DiskImageBuilder::new(kernel.to_path_buf());
    builder.set_boot_config(&boot_config);
    let (image, result) = match boot {
        Boot::Uefi => {
            let image = kernel.with_extension("uefi.img");
            let result = builder.create_uefi_image(&image);
            (image, result)
        }
        Boot::Bios => {
            let image = kernel.with_extension("bios.img");
            let result = builder.create_bios_image(&image);
            (image, result)
        }
    };
    result.unwrap_or_else(|e| panic!("failed to create disk image {}: {e:#}", image.display()));
    image
}

/// Downloads OVMF (UEFI firmware for QEMU) once and caches it next to the runner build
fn ovmf_firmware() -> Prebuilt {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ovmf");
    Prebuilt::fetch(Source::LATEST, &dir).expect("failed to fetch OVMF firmware")
}

/// What the kernel test framework reported over the serial port
#[derive(Debug, Default, PartialEq, Eq)]
struct TestSummary {
    /// `Running N tests` of the custom test runner, absent for `harness = false` tests
    expected: Option<usize>,
    passed: usize,
}

impl TestSummary {
    fn observe(&mut self, line: &str) {
        if let Some(count) = line
            .strip_prefix("Running ")
            .and_then(|rest| rest.strip_suffix(" tests"))
            .and_then(|count| count.parse().ok())
        {
            self.expected = Some(count);
        }
        self.passed += line.matches("[ok]").count();
    }
}

/// Prints the kernel serial output without the OVMF console noise
/// (terminal escape sequences and boot manager messages)
fn forward_serial_output(child: &mut Child) -> thread::JoinHandle<TestSummary> {
    let stdout = child.stdout.take().expect("qemu stdout is not piped");
    thread::spawn(move || {
        let mut out = std::io::stdout();
        let mut summary = TestSummary::default();
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            let stripped = strip_escape_sequences(&line);
            if !should_forward(&line, &stripped) {
                continue;
            }
            summary.observe(&String::from_utf8_lossy(&stripped));
            let _ = out.write_all(&stripped);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
        summary
    })
}

/// Drops firmware noise but keeps real empty lines printed by the kernel
fn should_forward(original: &[u8], stripped: &[u8]) -> bool {
    let blank = |line: &[u8]| line.iter().all(|&b| b == b'\r');
    if stripped.starts_with(b"BdsDxe:") {
        return false;
    }
    // a line that consisted of escape sequences only is a firmware screen update
    !(blank(stripped) && !blank(original))
}

fn strip_escape_sequences(line: &[u8]) -> Vec<u8> {
    const ESC: u8 = 0x1b;
    const BEL: u8 = 0x07;
    let mut result = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line[i] != ESC {
            result.push(line[i]);
            i += 1;
            continue;
        }
        i += 1;
        match line.get(i) {
            // CSI: ESC [ <params> <final byte 0x40..=0x7e>
            Some(b'[') => {
                i += 1;
                while i < line.len() && !(0x40..=0x7e).contains(&line[i]) {
                    i += 1;
                }
                i += 1;
            }
            // OSC: ESC ] <text> terminated by BEL or ESC \
            Some(b']') => {
                i += 1;
                while i < line.len() && line[i] != BEL && !(line[i] == ESC && line.get(i + 1) == Some(&b'\\')) {
                    i += 1;
                }
                i += if line.get(i) == Some(&ESC) { 2 } else { 1 };
            }
            // character set selection: ESC ( B and friends
            Some(b'(' | b')' | b'*' | b'+') => i += 2,
            // any other two byte sequence, e.g. ESC c, ESC =
            Some(_) => i += 1,
            None => {}
        }
    }
    result
}

/// Exit code of `cargo run`, signals are reported like a shell does
fn run_exit_code(status: ExitStatus) -> u8 {
    match (status.code(), status.signal()) {
        (Some(code), _) => code as u8,
        (None, Some(signal)) => 128u8.saturating_add(signal as u8),
        (None, None) => 1,
    }
}

/// `None` when QEMU was killed by the timeout
fn test_outcome(status: Option<ExitStatus>, summary: &TestSummary) -> Result<(), String> {
    let Some(status) = status else {
        return Err(format!("timed out after {}s", TEST_TIMEOUT.as_secs()));
    };
    if status.code() != Some(TEST_SUCCESS_EXIT_CODE) {
        return Err(format!("qemu exit code {:?}", status.code()));
    }
    match summary.expected {
        Some(expected) if summary.passed < expected => {
            Err(format!("only {} of {} tests reported [ok]", summary.passed, expected))
        }
        _ => Ok(()),
    }
}

fn wait_test(child: &mut Child) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("failed to wait for qemu") {
            return Some(status);
        }
        if started.elapsed() > TEST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(code: i32) -> Option<ExitStatus> {
        // wait(2) encoding: exit code in the second byte
        Some(ExitStatus::from_raw(code << 8))
    }

    #[test]
    fn boot_mode_parsing() {
        assert_eq!(parse_boot(None), Ok(Boot::Uefi));
        assert_eq!(parse_boot(Some("")), Ok(Boot::Uefi));
        assert_eq!(parse_boot(Some("uefi")), Ok(Boot::Uefi));
        assert_eq!(parse_boot(Some("bios")), Ok(Boot::Bios));
        assert!(parse_boot(Some("BIOS")).is_err());
        assert!(parse_boot(Some("efi")).is_err());
    }

    #[test]
    fn test_binary_detection() {
        assert!(!is_test_binary(Path::new("target/x86_64-unknown-none/debug/OSel")));
        assert!(is_test_binary(Path::new("deps/osel-0123456789abcdef")));
        assert!(is_test_binary(Path::new("out/basic_boot-0123456789ABCDEF")));
        assert!(is_test_binary(Path::new("out/heap-allocation-a1b2c3d4e5f60718")));
        assert!(!is_test_binary(Path::new("deps/osel-0123456789abcde")), "15 digits");
        assert!(!is_test_binary(Path::new("deps/osel-0123456789abcdef0")), "17 digits");
        assert!(!is_test_binary(Path::new("deps/x-ghijklmnopqrstuv")), "not hex");
        assert!(!is_test_binary(Path::new("")));
    }

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_escape_sequences(b"\x1b[2J\x1b[01;01Htext"), b"text");
        assert_eq!(strip_escape_sequences(b"a\x1b[=3hb"), b"ab");
        assert_eq!(strip_escape_sequences(b"plain"), b"plain");
    }

    #[test]
    fn strips_other_sequences() {
        assert_eq!(strip_escape_sequences(b"\x1b(Bok"), b"ok", "charset selection");
        assert_eq!(strip_escape_sequences(b"\x1b]0;title\x07ok"), b"ok", "OSC with BEL");
        assert_eq!(strip_escape_sequences(b"\x1b]0;title\x1b\\ok"), b"ok", "OSC with ST");
        assert_eq!(strip_escape_sequences(b"\x1bcok"), b"ok", "reset");
    }

    #[test]
    fn strips_truncated_sequences() {
        assert_eq!(strip_escape_sequences(b"text\x1b"), b"text");
        assert_eq!(strip_escape_sequences(b"text\x1b[12"), b"text");
        assert_eq!(strip_escape_sequences(b"text\x1b]0;tit"), b"text");
    }

    #[test]
    fn forwarding_keeps_kernel_blank_lines() {
        assert!(should_forward(b"", b""), "serial_println!() must survive");
        assert!(should_forward(b"\r", b"\r"));
        assert!(should_forward(b"[ok]", b"[ok]"));
        assert!(!should_forward(b"\x1b[2J\x1b[1;1H", b""), "pure screen update");
        assert!(!should_forward(b"BdsDxe: loading Boot0002", b"BdsDxe: loading Boot0002"));
    }

    #[test]
    fn summary_counts_tests() {
        let mut summary = TestSummary::default();
        for line in ["Running 3 tests", "a...\t[ok]", "b...\t[ok]", "garbage"] {
            summary.observe(line);
        }
        assert_eq!(summary, TestSummary { expected: Some(3), passed: 2 });
        summary.observe("Running x tests");
        assert_eq!(summary.expected, Some(3), "unparsable count is ignored");
    }

    #[test]
    fn outcome_success() {
        let summary = TestSummary { expected: Some(2), passed: 2 };
        assert_eq!(test_outcome(exit(33), &summary), Ok(()));
        let harness_free = TestSummary { expected: None, passed: 1 };
        assert_eq!(test_outcome(exit(33), &harness_free), Ok(()));
    }

    #[test]
    fn outcome_failures() {
        let summary = TestSummary { expected: Some(1), passed: 1 };
        assert!(test_outcome(exit(35), &summary).is_err(), "QemuExitCode::Failed");
        assert!(test_outcome(exit(0), &summary).is_err(), "triple fault / reset");
        assert!(test_outcome(exit(1), &summary).is_err());
        assert!(test_outcome(None, &summary).unwrap_err().contains("timed out"));
        let missing = TestSummary { expected: Some(3), passed: 1 };
        assert!(test_outcome(exit(33), &missing).unwrap_err().contains("1 of 3"));
    }

    #[test]
    fn run_exit_codes() {
        assert_eq!(run_exit_code(ExitStatus::from_raw(0)), 0);
        assert_eq!(run_exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(run_exit_code(ExitStatus::from_raw(9)), 137, "SIGKILL");
    }
}
