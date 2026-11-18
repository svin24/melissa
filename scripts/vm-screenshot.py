#!/usr/bin/env python3
"""Save a PNG of a running test VM's display via the QEMU monitor socket (stdlib only).

usage: vm-screenshot.py <desktop|headless> [out.png]   (default out: vm/<variant>/screen.png)
"""
import socket
import struct
import sys
import time
import zlib
from pathlib import Path


def ppm_to_png(ppm: bytes) -> bytes:
    magic, dims, maxval, pixels = ppm.split(b"\n", 3)
    if magic != b"P6" or maxval != b"255":
        raise ValueError("expected an 8-bit P6 PPM")
    width, height = map(int, dims.split())
    stride = width * 3
    raw = b"".join(b"\x00" + pixels[y * stride:(y + 1) * stride] for y in range(height))

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")


def main() -> None:
    variant = sys.argv[1]
    vm_dir = Path("vm") / variant
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else vm_dir / "screen.png"
    ppm = (vm_dir / "screen.ppm").resolve()
    ppm.unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as sock:
        sock.connect(str(vm_dir / "monitor.sock"))
        sock.sendall(f"screendump {ppm}\n".encode())
        for _ in range(50):
            time.sleep(0.2)
            if ppm.exists() and ppm.stat().st_size > 0:
                break
    time.sleep(0.5)
    out.write_bytes(ppm_to_png(ppm.read_bytes()))
    ppm.unlink()
    print(out)


if __name__ == "__main__":
    main()
