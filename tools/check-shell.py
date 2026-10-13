#!/usr/bin/env python3
"""Exercise the real kernel shell over COM1 and PS/2, then close headless QEMU."""

import argparse
import json
import os
import pathlib
import selectors
import signal
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--screenshot", help="optional QEMU PNG screenshot path")
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env.pop("OSEL_EXPECT", None)
    with tempfile.TemporaryDirectory(prefix="osel-shell-", dir="/tmp") as directory:
        address = directory + "/qmp.sock"
        process = subprocess.Popen(
            ["cargo", "run", "--", "-display", "none", "-qmp", f"unix:{address},server,nowait"],
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
            monitor = socket.socket(socket.AF_UNIX)
            monitor.settimeout(5)
            monitor.connect(address)
            stream = monitor.makefile("rwb", buffering=0)
            stream.readline()
            qmp("qmp_capabilities")
            command(b"help", b"No quotes")
            listing = command(b"ls /bin", b"clear\n")
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
