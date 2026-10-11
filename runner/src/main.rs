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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

/// `exit_qemu(QemuExitCode::Success)` makes QEMU exit with `(0x10 << 1) | 1`
const TEST_SUCCESS_EXIT_CODE: i32 = 33;
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Boot {
    Uefi,
    Bios,
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let kernel = PathBuf::from(args.next().expect("usage: osel-runner <kernel-elf> [qemu args...]"));
    let extra_qemu_args: Vec<_> = args.collect();

    let boot = match env::var("OSEL_BOOT").as_deref() {
        Ok("uefi") | Err(_) => Boot::Uefi,
        Ok("bios") => Boot::Bios,
        Ok(other) => panic!("unknown OSEL_BOOT value `{other}`, expected `uefi` or `bios`"),
    };
    let is_test = is_test_binary(&kernel);

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
    }
    qemu.args(&extra_qemu_args);
    qemu.stdout(Stdio::piped());

    let mut child = qemu.spawn().expect("failed to launch qemu-system-x86_64");
    let output = forward_serial_output(&mut child);
    let exit_code = if is_test { wait_test(&mut child) } else { wait_run(&mut child) };
    let _ = output.join();
    exit_code
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

/// Prints the kernel serial output without the OVMF console noise
/// (terminal escape sequences and boot manager messages)
fn forward_serial_output(child: &mut Child) -> thread::JoinHandle<()> {
    let stdout = child.stdout.take().expect("qemu stdout is not piped");
    thread::spawn(move || {
        let mut out = std::io::stdout();
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            let line = strip_escape_sequences(&line);
            if line.starts_with(b"BdsDxe:") || line.iter().all(|&b| b == b'\r') {
                continue;
            }
            let _ = out.write_all(&line);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
    })
}

fn strip_escape_sequences(line: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(line.len());
    let mut bytes = line.iter().copied();
    while let Some(b) = bytes.next() {
        if b == 0x1b {
            // CSI sequence: ESC [ <params> <final byte 0x40..=0x7e>
            if bytes.next() == Some(b'[') {
                for b in bytes.by_ref() {
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
            }
        } else {
            result.push(b);
        }
    }
    result
}

fn wait_run(child: &mut Child) -> ExitCode {
    let status = child.wait().expect("failed to wait for qemu");
    ExitCode::from(status.code().unwrap_or(1) as u8)
}

fn wait_test(child: &mut Child) -> ExitCode {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("failed to wait for qemu") {
            return match status.code() {
                Some(TEST_SUCCESS_EXIT_CODE) => ExitCode::SUCCESS,
                code => {
                    eprintln!("test failed: qemu exit code {code:?}");
                    ExitCode::FAILURE
                }
            };
        }
        if started.elapsed() > TEST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("test timed out after {}s", TEST_TIMEOUT.as_secs());
            return ExitCode::FAILURE;
        }
        sleep(Duration::from_millis(50));
    }
}
