#!/usr/bin/env python3
"""Exercise the real kernel shell over COM1 and PS/2, then close headless QEMU."""

import argparse
import json
import os
import pathlib
import re
import selectors
import signal
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--screenshot", help="optional QEMU PNG screenshot path")
    parser.add_argument("--boot-screenshot", help="optional automatic keyboard diagnostics screenshot")
    parser.add_argument("--image", type=pathlib.Path, help="test an existing disk image without rebuilding")
    parser.add_argument("--boot", choices=("uefi", "bios"), default=os.environ.get("OSEL_BOOT", "uefi"))
    parser.add_argument("--ovmf-code", type=pathlib.Path, help="UEFI firmware code for --image")
    parser.add_argument("--ovmf-vars", type=pathlib.Path, help="UEFI firmware variables for --image")
    parser.add_argument("--require-tetris", action="store_true", help="fail if Tetris is absent")
    parser.add_argument("--expected-version", help="require this version in the boot banner")
    parser.add_argument("--expected-commit", help="require this short commit in the boot banner")
    args = parser.parse_args()
    if args.image and args.boot == "uefi" and not (args.ovmf_code and args.ovmf_vars):
        parser.error("UEFI --image requires --ovmf-code and --ovmf-vars")
    if bool(args.expected_version) != bool(args.expected_commit):
        parser.error("--expected-version and --expected-commit must be used together")
    root = pathlib.Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env.pop("OSEL_EXPECT", None)
    env["OSEL_BOOT"] = args.boot
    with tempfile.TemporaryDirectory(prefix="osel-shell-", dir="/tmp") as directory:
        address = directory + "/qmp.sock"
        qemu_args = ["-display", "none", "-qmp", f"unix:{address},server,nowait"]
        if args.image:
            launch = [
                "qemu-system-x86_64", "-vga", "std", "-global", "VGA.xres=1280",
                "-global", "VGA.yres=1024", "-drive",
                f"format=raw,snapshot=on,file={args.image.resolve()}",
                "-serial", "stdio", "-no-reboot",
            ]
            if args.boot == "uefi":
                launch += [
                    "-drive", f"if=pflash,format=raw,readonly=on,file={args.ovmf_code.resolve()}",
                    "-drive", f"if=pflash,format=raw,snapshot=on,file={args.ovmf_vars.resolve()}",
                ]
        else:
            launch = ["cargo", "run", "--locked", "--"]
        process = subprocess.Popen(
            launch + qemu_args,
            cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, start_new_session=True,
        )
        transcript = bytearray()
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        monitor = None
        stream = None

        def wait(needle, start=0, timeout=60):
            deadline = time.monotonic() + timeout
            while needle not in transcript[start:]:
                if time.monotonic() >= deadline:
                    raise AssertionError(f"timeout waiting for {needle!r}")
                for key, _ in selector.select(0.2):
                    data = os.read(key.fd, 65536)
                    if not data:
                        raise AssertionError(f"QEMU stopped before {needle!r}")
                    transcript.extend(data)
            return bytes(transcript[start:])

        def send(data):
            # Stay below the emulated UART FIFO budget, including paste tests.
            for byte in data:
                process.stdin.write(bytes([byte]))
                process.stdin.flush()
                time.sleep(0.003)

        def command(line, expected):
            start = len(transcript)
            send(line + b"\n")
            result = wait(b"osel> ", start)
            assert expected in result, (line, expected, result)
            return result

        def qmp(name, arguments=None):
            stream.write((json.dumps({"execute": name, "arguments": arguments or {}}) + "\n").encode())
            while True:
                reply = json.loads(stream.readline())
                if "error" in reply:
                    raise RuntimeError(reply)
                if "return" in reply:
                    return reply["return"]

        try:
            wait(b"osel> ", timeout=180)
            assert b"OSel ready to go!" in transcript
            if args.expected_version:
                banner = f"OSel {args.expected_version} (commit {args.expected_commit}) woke up!"
                assert banner.encode() in transcript, (banner, bytes(transcript))
            monitor = socket.socket(socket.AF_UNIX)
            monitor.settimeout(5)
            monitor.connect(address)
            stream = monitor.makefile("rwb", buffering=0)
            stream.readline()
            qmp("qmp_capabilities")
            assert b"PS/2 init: Ready, error=None" in transcript
            # The watch must update without a working keyboard or any serial input.
            start = len(transcript)
            wait(b"KBD t=", start)
            line_start = transcript.index(b"KBD t=", start)
            wait(b"\n", line_start)
            samples = re.findall(rb"KBD t=(\d+)", transcript)
            assert int(samples[-1]) > int(samples[0]), samples
            if args.boot_screenshot:
                qmp("screendump", {"filename": str(pathlib.Path(args.boot_screenshot).resolve()), "format": "png"})
            command(b"kbd", b"PS/2 init: Ready, error=None")
            command(b"help", b"No quotes")
            listing = command(b"ls /bin", b"clear\n")
            if args.require_tetris:
                assert b"tetris\n" in listing, "required Tetris binary is missing from the image"
                command(b"cat /etc/font-license.txt", b"SIL OPEN FONT LICENSE")
            command(b"cat /etc/motd", b"Welcome to OSel!")
            command(b"missing-command", b"missing-command:")
            command(b"hellp\x08o", b"Hello from ring 3!")
            command(b"hello | clear", b"UnsupportedSyntax")
            start = len(transcript)
            send(b"never-run-this\x03")
            result = wait(b"osel> ", start)
            assert b"^C" in result and b"never-run-this:" not in result
            command(b"help\r", b"No quotes")
            command(b"x" * 260, b"command not executed")
            command(b"clear", b"\x1b[2J")
            if b"gohello\n" in listing:
                command(b"gohello", b"exit 30")
            if b"tetris\n" in listing:
                for seed in (2, 3):
                    start = len(transcript)
                    send(f"tetris --seed {seed} --trace\n".encode())
                    wait(b"TETRIS READY", start)
                    wait(b"TETRIS TICK", start)
                    send(b"a")
                    wait(b"TETRIS INPUT key=a changed=true", start)
                    send(b"x")
                    result = wait(b"osel> ", start)
                    assert b"TETRIS EXIT" in result
                    command(b"ls /bin", b"tetris\n")
            else:
                print("Tetris skipped: no Go binary in ramdisk")
            # Exercise the physical-keyboard path, not only the serial input.
            command(b"clear", b"\x1b[2J")
            start = len(transcript)
            for key in ["h", "e", "l", "p", "ret"]:
                qmp("human-monitor-command", {"command-line": "sendkey " + key + " 10"})
                time.sleep(0.04)
            result = wait(b"osel> ", start)
            assert b"No quotes" in result
            result = command(b"kbd", b"PS/2 init: Ready, error=None")
            counters = re.search(rb"irq=(\d+) bytes=(\d+) poll=(\d+) chars=(\d+)", result)
            assert counters and int(counters[1]) > 0 and int(counters[4]) >= 5, result
            if args.screenshot:
                qmp("screendump", {"filename": str(pathlib.Path(args.screenshot).resolve()), "format": "png"})
            qmp("quit")
            assert process.wait(timeout=10) == 0
            print("Shell OK: builtins, errors, editing, external programs, repeated Tetris, COM1 and PS/2")
        except Exception:
            print(transcript.decode("utf-8", errors="replace"))
            raise
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            if stream:
                stream.close()
            if monitor:
                monitor.close()
            selector.close()


if __name__ == "__main__":
    main()
