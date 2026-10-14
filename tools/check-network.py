#!/usr/bin/env python3
"""Play network Tetris from the real OSel shell with the actual local server."""
import argparse
import os
import pathlib
import selectors
import signal
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=pathlib.Path, help="test an existing image without rebuilding")
    parser.add_argument("--boot", choices=("uefi", "bios"), default=os.environ.get("OSEL_BOOT", "uefi"))
    parser.add_argument("--ovmf-code", type=pathlib.Path, help="UEFI firmware code for --image")
    parser.add_argument("--ovmf-vars", type=pathlib.Path, help="UEFI firmware variables for --image")
    args = parser.parse_args()
    if args.image and args.boot == "uefi" and not (args.ovmf_code and args.ovmf_vars):
        parser.error("UEFI --image requires --ovmf-code and --ovmf-vars")
    root = pathlib.Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="osel-network-", dir="/tmp") as directory:
        binary = pathlib.Path(directory) / "server"
        subprocess.run(["go", "build", "-mod=readonly", "-o", str(binary), "."],
                       cwd=root / "tools/network-fixture", check=True,
                       env={**os.environ, "GOTOOLCHAIN": "local"})
        server = subprocess.Popen([str(binary), "-bot"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        process = None
        transcript = bytearray()
        try:
            banner = server.stdout.readline().decode().strip()
            assert banner.startswith("NET SERVER 127.0.0.1:"), banner
            port = int(banner.rsplit(":", 1)[1])
            env = os.environ.copy()
            env.pop("OSEL_EXPECT", None)
            env["OSEL_BOOT"] = args.boot
            if args.image:
                nic = env.get("OSEL_NIC") or "e1000e"
                assert nic in ("e1000e", "rtl8139"), "network check requires e1000e or rtl8139"
                launch = ["qemu-system-x86_64", "-vga", "std", "-global", "VGA.xres=1280",
                          "-global", "VGA.yres=1024", "-drive",
                          f"format=raw,snapshot=on,file={args.image.resolve()}",
                          "-serial", "stdio", "-no-reboot", "-netdev", "user,id=net0",
                          "-device", f"{nic},netdev=net0"]
                if args.boot == "uefi":
                    launch += ["-drive", f"if=pflash,format=raw,readonly=on,file={args.ovmf_code.resolve()}",
                               "-drive", f"if=pflash,format=raw,snapshot=on,file={args.ovmf_vars.resolve()}"]
            else:
                launch = ["cargo", "run", "--locked", "--"]
            process = subprocess.Popen(launch + ["-display", "none"],
                                       cwd=root, env=env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)

            def wait(needle, start=0, timeout=90):
                deadline = time.monotonic() + timeout
                while needle not in transcript[start:]:
                    assert time.monotonic() < deadline, f"timeout waiting for {needle!r}"
                    for key, _ in selector.select(0.2):
                        data = os.read(key.fd, 65536)
                        assert data, f"QEMU stopped before {needle!r}"
                        transcript.extend(data)
                return bytes(transcript[start:])

            def send(data):
                for byte in data:
                    process.stdin.write(bytes([byte]))
                    process.stdin.flush()
                    time.sleep(0.005)

            wait(b"osel> ")
            start = len(transcript)
            send(b"net\n")
            result = wait(b"osel> ", start)
            expected_nic = b"Realtek RTL8139 C+" if env.get("OSEL_NIC") == "rtl8139" else b"Intel 82574L"
            assert expected_nic in result, result
            def command(line, expected):
                start = len(transcript)
                send(line + b"\n")
                result = wait(b"osel> ", start)
                assert expected in result, (line, expected, result)
                return result
            deadline = time.monotonic() + 5
            while True:
                start = len(transcript)
                send(b"net ip\n")
                result = wait(b"osel> ", start)
                if b"DHCP configured=true" in result:
                    break
                assert time.monotonic() < deadline, result
                time.sleep(0.1)
            command(b"net route", b"default via 10.0.2.2")
            command(b"net dns", b"DNS 10.0.2.3")
            command(b"ping gateway 1", b"received, 0% loss")
            command(b"ping 10.0.2.250 1", b"100% loss")
            start = len(transcript)
            send(b"ping gateway 10\n")
            wait(b"seq=1", start)
            start = len(transcript)
            send(b"\x03")
            wait(b"osel> ", start, timeout=2)
            command(b"nslookup a..b", b"EINVAL")
            command(f"tcpcheck 10.0.2.2 {port}".encode(), b"connected in")
            for _ in range(2):
                start = len(transcript)
                send(f"tetris --network --server 10.0.2.2:{port} --create --trace\n".encode())
                wait(b"NETWORK CONNECTED", start)
                wait(b"NETWORK FIELD type=0", start)
                wait(b"NETWORK FIELD type=1", start)
                send(b"s")
                wait(b"NETWORK INPUT key=s", start)
                wait(b"NETWORK PONG tetris", start)
                send(b"x")
                wait(b"NETWORK EXIT", start)
                wait(b"osel> ", start)
            server.terminate()
            server.wait(timeout=10)
            command(f"tcpcheck 10.0.2.2 {port}".encode(), b"ECONNREFUSED")
            start = len(transcript)
            send(f"tetris --network --server 10.0.2.2:{port} --create\n".encode())
            result = wait(b"osel> ", start)
            assert b"Tetris server unavailable" in result, result
            assert b"exit 1" in result, result
            print("Network shell OK: DHCP/routes/DNS, ICMP replies/timeout/Ctrl+C, TCP port, two games and offline-server recovery")
        except Exception:
            print(transcript.decode(errors="replace"))
            raise
        finally:
            if process and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            if server.poll() is None:
                server.kill()
                server.wait()


if __name__ == "__main__":
    main()
