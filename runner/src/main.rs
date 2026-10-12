//! Cargo runner for the kernel: packs the kernel ELF into a bootable disk image
//! and boots it in QEMU. Replaces `bootimage runner` from bootloader 0.9 times.
//!
//! Usage: osel-runner <kernel-elf> [extra qemu args...]
//! Env:
//! - `OSEL_BOOT=uefi|bios`: firmware to boot with (default: uefi)
//! - `OSEL_EXPECT=<text>`: run headless and succeed once the kernel prints `<text>`
//!   to the serial port (a smoke test of the real kernel)
//! - `OSEL_TIMEOUT=<seconds>`: limit for tests and `OSEL_EXPECT` runs (default: 60)
//! - `OSEL_OVMF_DIR=<dir>`: where to cache the UEFI firmware (default: runner/.ovmf)
//! - `OSEL_RAMDISK=<dir>`: directory packed into the ramdisk as a cpio archive
//!   (default: ramdisk/ of the repository; relative paths start there too). The programs
//!   of user/ are built and added as /bin/<name>.

use bootloader::{BootConfig, DiskImageBuilder};
use bootloader_boot_config::LevelFilter;
use osel_core::cpio::CpioBuilder;
use ovmf_prebuilt::{Arch, FileType, Prebuilt, Source};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

/// `exit_qemu(QemuExitCode::Success)` makes QEMU exit with `(0x10 << 1) | 1`
const TEST_SUCCESS_EXIT_CODE: i32 = 33;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Test kernels rely on a known amount of memory
const TEST_MEMORY: &str = "128M";
/// What every kernel finds in its ramdisk, relative to the repository
const DEFAULT_RAMDISK_DIR: &str = "ramdisk";
/// Where the bootloader looks for the ramdisk in the image, `RAMDISK_FILE_NAME` of `bootloader`
const RAMDISK_FILE_NAME: &str = "ramdisk";
/// Programs in user/ of the repository, each a crate of its own, added as /bin/<name>
const USER_PROGRAMS: &[&str] = &["hello"];
/// Their target: `ET_EXEC` at a fixed address, the kernel loads no position independent code
const USER_TARGET: &str = "x86_64-unknown-none";
/// Files the host leaves behind that must not make the ramdisk differ between machines
const IGNORED_FILES: &[&str] = &[".DS_Store"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boot {
    Uefi,
    Bios,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// Interactive run with a window
    Run,
    /// A test kernel: exits QEMU itself through isa-debug-exit
    Test,
    /// Headless run of the real kernel until it prints the expected text
    Expect(String),
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let kernel = PathBuf::from(args.next().expect("usage: osel-runner <kernel-elf> [qemu args...]"));
    let mut extra_qemu_args: Vec<_> = args.collect();

    let config = parse_boot(env::var("OSEL_BOOT").ok().as_deref()).and_then(|boot| {
        let timeout = parse_timeout(env::var("OSEL_TIMEOUT").ok().as_deref())?;
        let ramdisk_dir = ramdisk_dir(env::var_os("OSEL_RAMDISK").as_deref(), &repository());
        let programs = build_user_programs(&repository())?;
        let ramdisk = pack_ramdisk(&ramdisk_dir, &programs)?;
        Ok((boot, timeout, ramdisk))
    });
    let (boot, timeout, ramdisk) = match config {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let mode = if is_test_binary(&kernel) {
        Mode::Test
    } else {
        env::var("OSEL_EXPECT").ok().filter(|text| !text.is_empty()).map_or(Mode::Run, Mode::Expect)
    };
    if mode == Mode::Test && !extra_qemu_args.is_empty() {
        // `cargo test -- <args>` would otherwise hand test filters to QEMU
        eprintln!("warning: test arguments are not supported and ignored: {extra_qemu_args:?}");
        extra_qemu_args.clear();
    }

    let image = image_path(&kernel, boot);
    create_disk_image(&kernel, &image, boot, &ramdisk);

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
    match mode {
        Mode::Run => {}
        Mode::Test => {
            qemu.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-display", "none", "-m", TEST_MEMORY]);
        }
        Mode::Expect(_) => {
            qemu.args(["-display", "none"]);
        }
    }
    if mode != Mode::Run {
        // nothing reads the host keyboard, and QEMU must not switch the terminal to raw mode
        qemu.stdin(Stdio::null());
    }
    qemu.args(&extra_qemu_args);
    qemu.stdout(Stdio::piped());

    let mut child = qemu.spawn().expect("failed to launch qemu-system-x86_64");
    let stdout = child.stdout.take().expect("qemu stdout is not piped");
    let exit_code = match &mode {
        Mode::Run => {
            let output = thread::spawn(move || forward_serial_output(BufReader::new(stdout), std::io::stdout(), None));
            let status = child.wait().expect("failed to wait for qemu");
            let _ = output.join();
            ExitCode::from(run_exit_code(status))
        }
        Mode::Test => {
            let output = thread::spawn(move || forward_serial_output(BufReader::new(stdout), std::io::stdout(), None));
            let status = wait_with_timeout(&mut child, timeout, None);
            let summary = output.join().unwrap_or_default();
            report(test_outcome(status, &summary, timeout))
        }
        Mode::Expect(text) => {
            let (found_tx, found_rx) = mpsc::channel();
            let watch = Some((text.to_string(), found_tx));
            let output = thread::spawn(move || forward_serial_output(BufReader::new(stdout), std::io::stdout(), watch));
            let status = wait_with_timeout(&mut child, timeout, Some(&found_rx));
            let found = found_rx.try_recv().is_ok() || output.join().is_ok_and(|summary| summary.found);
            report(expect_outcome(status, found, text, timeout))
        }
    };
    if mode == Mode::Test {
        // test images are only needed for the single run, they would pile up in target/
        let _ = std::fs::remove_file(&image);
    }
    exit_code
}

fn report(outcome: Result<(), String>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse_boot(value: Option<&str>) -> Result<Boot, String> {
    match value {
        None | Some("") | Some("uefi") => Ok(Boot::Uefi),
        Some("bios") => Ok(Boot::Bios),
        Some(other) => Err(format!("unknown OSEL_BOOT value `{other}`, expected `uefi` or `bios`")),
    }
}

fn parse_timeout(value: Option<&str>) -> Result<Duration, String> {
    match value {
        None | Some("") => Ok(DEFAULT_TIMEOUT),
        Some(seconds) => match seconds.parse::<u64>() {
            Ok(seconds) if seconds > 0 => Ok(Duration::from_secs(seconds)),
            _ => Err(format!("OSEL_TIMEOUT must be a positive number of seconds, got `{seconds}`")),
        },
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

/// The disk image goes next to the kernel ELF, one per boot mode
fn image_path(kernel: &Path, boot: Boot) -> PathBuf {
    let mut name = kernel.file_name().unwrap_or_default().to_os_string();
    name.push(match boot {
        Boot::Uefi => ".uefi.img",
        Boot::Bios => ".bios.img",
    });
    kernel.with_file_name(name)
}

fn create_disk_image(kernel: &Path, image: &Path, boot: Boot, ramdisk: &[u8]) {
    let mut boot_config = BootConfig::default();
    // the bootloader traces every boot step by default, keep only problems
    boot_config.log_level = LevelFilter::Warn;

    let mut builder = DiskImageBuilder::new(kernel.to_path_buf());
    builder.set_boot_config(&boot_config);
    // `set_ramdisk` takes a path, this is the same file `ramdisk` without a temporary copy
    builder.set_file_contents(RAMDISK_FILE_NAME.to_string(), ramdisk.to_vec());
    let result = match boot {
        Boot::Uefi => builder.create_uefi_image(image),
        Boot::Bios => builder.create_bios_image(image),
    };
    result.unwrap_or_else(|e| panic!("failed to create disk image {}: {e:#}", image.display()));
}

/// The repository the runner belongs to
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the runner lives in the repository").to_path_buf()
}

/// The directory to pack into the ramdisk: `OSEL_RAMDISK` or ramdisk/, relative to the
/// repository, so that the result does not depend on where cargo was started
fn ramdisk_dir(value: Option<&std::ffi::OsStr>, repository: &Path) -> PathBuf {
    match value {
        Some(dir) if !dir.is_empty() => repository.join(dir),
        _ => repository.join(DEFAULT_RAMDISK_DIR),
    }
}

/// Builds every program of `USER_PROGRAMS` and returns its name and bytes. A nested cargo
/// with its own target directory: the kernel build that started the runner holds target/.
fn build_user_programs(repository: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let target_dir = repository.join("user/target");
    let mut programs = Vec::new();
    for name in USER_PROGRAMS {
        let manifest = repository.join("user").join(name).join("Cargo.toml");
        let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["build", "--quiet", "--release", "--target", USER_TARGET])
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target_dir)
            // what the outer cargo set for the kernel must not leak into the program
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET")
            .env("CARGO_ENCODED_RUSTFLAGS", "-Crelocation-model=static")
            .status()
            .map_err(|e| format!("cannot start cargo to build user/{name}: {e}"))?;
        if !status.success() {
            return Err(format!("building user/{name} failed"));
        }
        let binary = target_dir.join(USER_TARGET).join("release").join(name);
        let data = std::fs::read(&binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
        programs.push((name.to_string(), data));
    }
    Ok(programs)
}

/// Packs `dir` into a cpio archive: directories and regular files with their
/// permission bits, sorted by name, so the same tree always gives the same bytes.
/// `programs` go to /bin after it; a file of the same name in `dir` is an error.
fn pack_ramdisk(dir: &Path, programs: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    if !dir.is_dir() {
        return Err(format!("ramdisk directory {} does not exist, see OSEL_RAMDISK", dir.display()));
    }
    let mut builder = CpioBuilder::new();
    let mut packed = Vec::new();
    pack_dir(dir, "", &mut builder, &mut packed)?;
    if !programs.is_empty() && !packed.iter().any(|name| name == "bin") {
        builder.add_dir("bin", 0o755);
    }
    for (name, data) in programs {
        let path = format!("bin/{name}");
        if packed.contains(&path) {
            return Err(format!("{path} is in the ramdisk directory and also a program of user/"));
        }
        builder.add_file(&path, 0o755, data);
    }
    Ok(builder.finish())
}

fn pack_dir(dir: &Path, prefix: &str, builder: &mut CpioBuilder, packed: &mut Vec<String>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let read_error = |path: &Path, e: std::io::Error| format!("cannot read {}: {e}", path.display());
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| read_error(dir, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| read_error(dir, e))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("ramdisk file name {name:?} is not UTF-8"))?;
        if IGNORED_FILES.contains(&name.as_str()) {
            continue;
        }
        let name = format!("{prefix}{name}");
        packed.push(name.clone());
        // symlinks are not followed: the kernel has no symlinks yet
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| read_error(&path, e))?;
        let permissions = metadata.permissions().mode() & 0o7777;
        if metadata.is_dir() {
            builder.add_dir(&name, permissions);
            pack_dir(&path, &format!("{name}/"), builder, packed)?;
        } else if metadata.is_file() {
            if metadata.len() > u64::from(u32::MAX) {
                return Err(format!("{} is 4 GiB or more, cpio can't store it", path.display()));
            }
            let data = std::fs::read(&path).map_err(|e| read_error(&path, e))?;
            builder.add_file(&name, permissions, &data);
        } else {
            return Err(format!("{}: only files and directories can go to the ramdisk", path.display()));
        }
    }
    Ok(())
}

/// Downloads OVMF (UEFI firmware for QEMU) once. Kept outside target/ so that
/// `cargo clean` and the CI cache cleanup don't throw it away.
fn ovmf_firmware() -> Prebuilt {
    let dir = env::var_os("OSEL_OVMF_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(".ovmf"));
    Prebuilt::fetch(Source::LATEST, &dir).expect("failed to fetch OVMF firmware")
}

/// What the kernel reported over the serial port
#[derive(Debug, Default, PartialEq, Eq)]
struct TestSummary {
    /// `Running N tests` of the custom test runner, absent for `harness = false` tests
    expected: Option<usize>,
    /// Lines ending with `[ok]`
    passed: usize,
    /// The `OSEL_EXPECT` text was seen
    found: bool,
}

impl TestSummary {
    fn observe(&mut self, line: &str) {
        // firmware output without a newline can precede the kernel text on the same line
        if let Some(count) = line
            .find("Running ")
            .map(|start| &line[start + "Running ".len()..])
            .and_then(|rest| rest.strip_suffix(" tests").or_else(|| rest.trim_end().strip_suffix(" tests")))
            .and_then(|count| count.parse().ok())
        {
            self.expected = Some(count);
        }
        // only a test result counts, not `[ok]` mentioned inside some message
        if line.trim_end().ends_with("[ok]") {
            self.passed += 1;
        }
    }
}

/// Copies the kernel serial output without the OVMF console noise (terminal escape
/// sequences and boot manager messages) and summarizes it. With `watch`, reports
/// through the channel as soon as a line contains the watched text.
fn forward_serial_output(
    reader: impl BufRead,
    mut out: impl Write,
    watch: Option<(String, mpsc::Sender<()>)>,
) -> TestSummary {
    let mut summary = TestSummary::default();
    for line in reader.split(b'\n') {
        let Ok(line) = line else { break };
        let stripped = strip_escape_sequences(&line);
        if !should_forward(&line, &stripped) {
            continue;
        }
        let text = String::from_utf8_lossy(&stripped);
        summary.observe(&text);
        if let Some((expected, found)) = &watch
            && text.contains(expected.as_str())
        {
            summary.found = true;
            let _ = found.send(());
        }
        let _ = out.write_all(&stripped);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
    summary
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

/// `status` is `None` when QEMU was killed by the timeout
fn test_outcome(status: Option<ExitStatus>, summary: &TestSummary, timeout: Duration) -> Result<(), String> {
    let Some(status) = status else {
        return Err(format!("timed out after {}s", timeout.as_secs()));
    };
    if status.code() != Some(TEST_SUCCESS_EXIT_CODE) {
        return Err(format!("qemu exit code {:?}", status.code()));
    }
    match summary.expected {
        Some(expected) if summary.passed != expected => {
            Err(format!("{} of {} tests reported [ok]", summary.passed, expected))
        }
        // a test kernel that exits with success before reporting anything is broken
        None if summary.passed == 0 => Err("no test reported [ok]".into()),
        _ => Ok(()),
    }
}

/// `status` is `None` when QEMU was stopped: by the runner after finding the text,
/// or by the timeout
fn expect_outcome(status: Option<ExitStatus>, found: bool, expected: &str, timeout: Duration) -> Result<(), String> {
    match (found, status) {
        (true, _) => Ok(()),
        (false, None) => Err(format!("`{expected}` not printed within {}s", timeout.as_secs())),
        (false, Some(status)) => Err(format!("kernel stopped with {status} before printing `{expected}`")),
    }
}

/// Waits for `child` to exit. Kills it after `timeout`, or as soon as `stop` receives
/// a message; returns `None` then.
fn wait_with_timeout(child: &mut Child, timeout: Duration, stop: Option<&mpsc::Receiver<()>>) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("failed to wait for qemu") {
            return Some(status);
        }
        let stop_requested = stop.is_some_and(|stop| stop.try_recv().is_ok());
        if stop_requested || started.elapsed() > timeout {
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
    use std::io::Cursor;

    fn exit(code: i32) -> Option<ExitStatus> {
        // wait(2) encoding: exit code in the second byte
        Some(ExitStatus::from_raw(code << 8))
    }

    fn summary(expected: Option<usize>, passed: usize) -> TestSummary {
        TestSummary { expected, passed, found: false }
    }

    fn forward(input: &[u8], watch: Option<&str>) -> (String, TestSummary) {
        let mut out = Vec::new();
        let (tx, _rx) = mpsc::channel();
        let summary = forward_serial_output(Cursor::new(input), &mut out, watch.map(|w| (w.to_string(), tx)));
        (String::from_utf8_lossy(&out).into_owned(), summary)
    }

    const TIMEOUT: Duration = Duration::from_secs(60);

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
    fn timeout_parsing() {
        assert_eq!(parse_timeout(None), Ok(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some("")), Ok(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some("120")), Ok(Duration::from_secs(120)));
        assert!(parse_timeout(Some("0")).is_err());
        assert!(parse_timeout(Some("-1")).is_err());
        assert!(parse_timeout(Some("1m")).is_err());
    }

    #[test]
    fn ramdisk_dir_is_relative_to_the_repository() {
        let repository = Path::new("/repo");
        let dir = |value: &str| ramdisk_dir(Some(std::ffi::OsStr::new(value)), repository);
        assert_eq!(ramdisk_dir(None, repository), Path::new("/repo/ramdisk"));
        assert_eq!(dir(""), Path::new("/repo/ramdisk"));
        assert_eq!(dir("tests/data"), Path::new("/repo/tests/data"));
        assert_eq!(dir("/abs/dir"), Path::new("/abs/dir"));
    }

    /// A fresh directory under the system temp dir, removed when dropped
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = env::temp_dir().join(format!("osel-runner-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mount(archive: Vec<u8>) -> osel_core::cpio::CpioFs {
        use osel_core::block::MemBlockDevice;
        osel_core::cpio::CpioFs::mount(std::sync::Arc::new(MemBlockDevice::new(archive))).unwrap()
    }

    #[test]
    fn packs_a_directory_tree() {
        use osel_core::vfs::{FileSystem, list_dir, read_to_end, resolve_absolute as resolve};
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("pack");
        std::fs::create_dir_all(dir.0.join("etc/deep")).unwrap();
        std::fs::create_dir(dir.0.join("empty")).unwrap();
        std::fs::write(dir.0.join("etc/motd"), b"hello\n").unwrap();
        std::fs::write(dir.0.join("etc/deep/data"), vec![7u8; 3000]).unwrap();
        std::fs::write(dir.0.join("run"), b"#!").unwrap();
        std::fs::set_permissions(dir.0.join("run"), std::fs::Permissions::from_mode(0o750)).unwrap();
        std::fs::write(dir.0.join("suid"), b"").unwrap();
        std::fs::set_permissions(dir.0.join("suid"), std::fs::Permissions::from_mode(0o4755)).unwrap();
        std::fs::write(dir.0.join(".DS_Store"), b"finder").unwrap();

        let archive = pack_ramdisk(&dir.0, &[]).unwrap();
        assert_eq!(archive, pack_ramdisk(&dir.0, &[]).unwrap(), "packing is deterministic");
        let fs = mount(archive);
        let root = fs.root();
        let read = |path| read_to_end(&*resolve(&root, path).unwrap()).unwrap();
        assert_eq!(read("/etc/motd"), b"hello\n");
        assert_eq!(read("/etc/deep/data"), vec![7u8; 3000]);
        assert_eq!(resolve(&root, "/run").unwrap().stat().permissions, 0o750);
        assert_eq!(resolve(&root, "/suid").unwrap().stat().permissions, 0o4755, "setuid survives");
        let names: Vec<String> = list_dir(&*root).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["empty", "etc", "run", "suid"], ".DS_Store is skipped");
        assert!(list_dir(&*resolve(&root, "/empty").unwrap()).unwrap().is_empty());
    }

    #[test]
    fn programs_go_to_bin() {
        use osel_core::vfs::{FileSystem, read_to_end, resolve_absolute as resolve};

        let programs = [("hello".to_string(), b"\x7fELF".to_vec())];
        let dir = TempDir::new("programs");
        let fs = mount(pack_ramdisk(&dir.0, &programs).unwrap());
        let hello = resolve(&fs.root(), "/bin/hello").unwrap();
        assert_eq!(read_to_end(&*hello).unwrap(), b"\x7fELF");
        assert_eq!(hello.stat().permissions, 0o755);

        // a bin/ of the ramdisk directory gets the programs too
        std::fs::create_dir(dir.0.join("bin")).unwrap();
        std::fs::write(dir.0.join("bin/tool"), b"x").unwrap();
        let fs = mount(pack_ramdisk(&dir.0, &programs).unwrap());
        assert!(resolve(&fs.root(), "/bin/tool").is_ok() && resolve(&fs.root(), "/bin/hello").is_ok());

        std::fs::write(dir.0.join("bin/hello"), b"mine").unwrap();
        let error = pack_ramdisk(&dir.0, &programs).unwrap_err();
        assert!(error.contains("bin/hello"), "{error}");
    }

    #[test]
    fn empty_directory_gives_an_empty_archive() {
        let dir = TempDir::new("empty");
        let fs = mount(pack_ramdisk(&dir.0, &[]).unwrap());
        assert_eq!(fs.node_count(), 1);
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = TempDir::new("missing");
        let error = pack_ramdisk(&dir.0.join("nope"), &[]).unwrap_err();
        assert!(error.contains("does not exist") && error.contains("OSEL_RAMDISK"), "{error}");
        std::fs::write(dir.0.join("file"), b"").unwrap();
        assert!(pack_ramdisk(&dir.0.join("file"), &[]).is_err(), "a file is not a directory");
    }

    #[test]
    fn symlinks_are_refused() {
        let dir = TempDir::new("symlink");
        std::fs::write(dir.0.join("target"), b"x").unwrap();
        std::os::unix::fs::symlink("target", dir.0.join("link")).unwrap();
        let error = pack_ramdisk(&dir.0, &[]).unwrap_err();
        assert!(error.contains("only files and directories"), "{error}");
    }

    #[test]
    fn the_repository_ramdisk_packs() {
        let fs = mount(pack_ramdisk(&ramdisk_dir(None, &repository()), &[]).unwrap());
        assert!(fs.node_count() > 1, "ramdisk/ of the repository is empty");
    }

    #[test]
    fn test_binary_detection() {
        assert!(!is_test_binary(Path::new("target/x86_64-unknown-none/debug/OSel")));
        assert!(!is_test_binary(Path::new("target/x86_64-unknown-none/release/OSel")));
        assert!(is_test_binary(Path::new("target/x86_64-unknown-none/debug/deps/osel-0123456789abcdef")));
        assert!(is_test_binary(Path::new(
            "target/x86_64-unknown-none/debug/build/osel/af4e2a3f9a893cec/out/osel-af4e2a3f9a893cec"
        )));
        assert!(is_test_binary(Path::new("out/basic_boot-0123456789ABCDEF")));
        assert!(is_test_binary(Path::new("out/heap-allocation-a1b2c3d4e5f60718")));
        assert!(!is_test_binary(Path::new("deps/osel-0123456789abcde")), "15 digits");
        assert!(!is_test_binary(Path::new("deps/osel-0123456789abcdef0")), "17 digits");
        assert!(!is_test_binary(Path::new("deps/x-ghijklmnopqrstuv")), "not hex");
        assert!(!is_test_binary(Path::new("")));
    }

    #[test]
    fn image_names() {
        let kernel = Path::new("out/heap_stress-5bff53b1c56fbc6d");
        assert_eq!(image_path(kernel, Boot::Uefi), Path::new("out/heap_stress-5bff53b1c56fbc6d.uefi.img"));
        assert_eq!(image_path(kernel, Boot::Bios), Path::new("out/heap_stress-5bff53b1c56fbc6d.bios.img"));
        assert_eq!(image_path(Path::new("debug/OSel"), Boot::Uefi), Path::new("debug/OSel.uefi.img"));
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
        assert_eq!(strip_escape_sequences(b"\x1b)0ok"), b"ok");
        assert_eq!(strip_escape_sequences(b"\x1b*Bok"), b"ok");
        assert_eq!(strip_escape_sequences(b"\x1b+Bok"), b"ok");
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
        assert!(should_forward(b"kernel says BdsDxe: hi", b"kernel says BdsDxe: hi"), "only at the start");
    }

    #[test]
    fn summary_counts_tests() {
        let mut summary = TestSummary::default();
        for line in ["Running 3 tests", "a...\t[ok]", "b...\t[ok]  ", "garbage"] {
            summary.observe(line);
        }
        assert_eq!((summary.expected, summary.passed), (Some(3), 2));
        summary.observe("Running x tests");
        assert_eq!(summary.expected, Some(3), "unparsable count is ignored");
    }

    #[test]
    fn summary_finds_count_after_firmware_text() {
        let mut summary = TestSummary::default();
        summary.observe("BdsDxe: starting Boot0002Running 4 tests");
        assert_eq!(summary.expected, Some(4));
    }

    #[test]
    fn summary_ignores_ok_inside_messages() {
        let mut summary = TestSummary::default();
        summary.observe("Error: expected [ok] but got nothing");
        summary.observe("[ok] at the start is not a result either");
        assert_eq!(summary.passed, 0);
    }

    #[test]
    fn forwarding_end_to_end() {
        let input = b"\x1b[2J\x1b[1;1HBdsDxe: loading\nRunning 2 tests\na...\t[ok]\n\nb...\t[ok]";
        let (out, summary) = forward(input, None);
        assert_eq!(out, "Running 2 tests\na...\t[ok]\n\nb...\t[ok]\n", "last partial line is kept");
        assert_eq!((summary.expected, summary.passed), (Some(2), 2));
    }

    #[test]
    fn forwarding_survives_invalid_utf8() {
        let (out, summary) = forward(b"\xff\xfe broken\nx...\t[ok]\n", None);
        assert!(out.contains("broken"));
        assert_eq!(summary.passed, 1);
    }

    #[test]
    fn forwarding_watches_for_text() {
        let (_, summary) = forward(b"OSel woke up!\nOSel ready to go!\n", Some("ready to go"));
        assert!(summary.found);
        let (_, summary) = forward(b"OSel woke up!\n", Some("ready to go"));
        assert!(!summary.found);
    }

    #[test]
    fn outcome_success() {
        assert_eq!(test_outcome(exit(33), &summary(Some(2), 2), TIMEOUT), Ok(()));
        assert_eq!(test_outcome(exit(33), &summary(None, 1), TIMEOUT), Ok(()), "harness = false");
        assert_eq!(test_outcome(exit(33), &summary(Some(0), 0), TIMEOUT), Ok(()), "a test binary without tests");
    }

    #[test]
    fn outcome_failures() {
        let ok = summary(Some(1), 1);
        assert!(test_outcome(exit(35), &ok, TIMEOUT).is_err(), "QemuExitCode::Failed");
        assert!(test_outcome(exit(0), &ok, TIMEOUT).is_err(), "triple fault / reset");
        assert!(test_outcome(exit(1), &ok, TIMEOUT).is_err());
        assert!(test_outcome(None, &ok, TIMEOUT).unwrap_err().contains("timed out after 60s"));
        assert!(test_outcome(exit(33), &summary(Some(3), 1), TIMEOUT).unwrap_err().contains("1 of 3"));
        assert!(test_outcome(exit(33), &summary(Some(1), 2), TIMEOUT).unwrap_err().contains("2 of 1"), "extra [ok]");
        assert!(test_outcome(exit(33), &summary(None, 0), TIMEOUT).is_err(), "silent success");
    }

    #[test]
    fn expect_outcomes() {
        assert_eq!(expect_outcome(None, true, "ready", TIMEOUT), Ok(()));
        assert_eq!(expect_outcome(exit(0), true, "ready", TIMEOUT), Ok(()));
        assert!(expect_outcome(None, false, "ready", TIMEOUT).unwrap_err().contains("within 60s"));
        assert!(expect_outcome(exit(0), false, "ready", TIMEOUT).unwrap_err().contains("stopped"));
    }

    #[test]
    fn run_exit_codes() {
        assert_eq!(run_exit_code(ExitStatus::from_raw(0)), 0);
        assert_eq!(run_exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(run_exit_code(ExitStatus::from_raw(9)), 137, "SIGKILL");
    }

    #[test]
    fn timeout_kills_the_process() {
        let mut child = Command::new("sleep").arg("10").spawn().unwrap();
        let started = Instant::now();
        assert_eq!(wait_with_timeout(&mut child, Duration::from_millis(200), None), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(child.try_wait().unwrap().is_some(), "the process is reaped");
    }

    #[test]
    fn stop_request_kills_the_process() {
        let mut child = Command::new("sleep").arg("10").spawn().unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send(()).unwrap();
        assert_eq!(wait_with_timeout(&mut child, Duration::from_secs(30), Some(&rx)), None);
    }

    #[test]
    fn finished_process_reports_status() {
        let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        let status = wait_with_timeout(&mut child, Duration::from_secs(30), None).unwrap();
        assert_eq!(status.code(), Some(7));
    }
}
