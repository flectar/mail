#!/usr/bin/env python3
"""Exercise the packaged PDF worker without initializing UI or account databases.

Usage: python3 scripts/test-pdf-preview.py target/debug/flectar-mail [output.png]
Creates an original two-page vector/text fixture and checks page selection,
colors, dimensions, malformed inputs and protocol limits. Requires staged PDFium.
"""
import pathlib
import struct
import subprocess
import sys
import time
import zlib


def fixture():
    streams = [b"0.1 0.4 0.8 rg 20 20 160 100 re f BT /F1 16 Tf 20 150 Td (Flectar PDF preview) Tj ET", b"0.8 0.2 0.1 rg 20 20 160 100 re f BT /F1 16 Tf 20 150 Td (Second page) Tj ET"]
    objects = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"]
    for content in (6, 7):
        objects.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents {content} 0 R >>".encode())
    objects.append(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    objects += [b"<< /Length " + str(len(s)).encode() + b" >>\nstream\n" + s + b"\nendstream" for s in streams]
    data = b"%PDF-1.4\n"; offsets = [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(data)); data += f"{i} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(data)
    data += f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode()
    data += b"".join(f"{offset:010} 00000 n \n".encode() for offset in offsets[1:])
    return data + f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()


def png(path, width, height, pixels):
    def chunk(kind, value):
        return struct.pack(">I", len(value)) + kind + value + struct.pack(">I", zlib.crc32(kind + value))
    raw = b"".join(b"\0" + pixels[y * width * 4:(y + 1) * width * 4] for y in range(height))
    pathlib.Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def check(binary, output=None):
    data = fixture()
    for index, zoom in [(0, 100), (1, 100), (0, 200)]:
        start = time.monotonic()
        result = subprocess.run([binary, "--render-pdf-page"], input=struct.pack("<III", index, zoom, len(data)) + data, capture_output=True, timeout=22)
        assert result.returncode == 0, (result.returncode, result.stderr.decode(errors="replace"))
        magic, count, width, height = struct.unpack("<4sIII", result.stdout[:16])
        pixels = result.stdout[16:]
        assert magic == b"FPD1" and count == 2
        assert 0 < width <= 2880 and 0 < height <= 2880
        assert len(pixels) == width * height * 4
        center = pixels[(height // 2 * width + width // 2) * 4:][:4]
        expected = (26, 102, 204, 255) if index == 0 else (204, 51, 26, 255)
        assert all(abs(a - b) <= 1 for a, b in zip(center, expected)), center
        print(f"Page {index + 1} at {zoom}%: {width}x{height}, {time.monotonic() - start:.3f}s")
        if output and index == 0 and zoom == 100: png(output, width, height, pixels)
    for payload in [struct.pack("<III", 9, 100, len(data)) + data, struct.pack("<III", 0, 100, 4) + b"oops", struct.pack("<III", 0, 100, 16 * 1024 * 1024 + 1), struct.pack("<III", 0, 999, 0), b"short"]:
        result = subprocess.run([binary, "--render-pdf-page"], input=payload, capture_output=True, timeout=22)
        assert result.returncode != 0 and not result.stdout
    print("PASS: pages, zoom, bitmap bounds, colors, malformed input and oversized requests")


if __name__ == "__main__":
    if sys.argv[1] == "--fixture":
        pathlib.Path(sys.argv[2]).write_bytes(fixture())
    else:
        check(str(pathlib.Path(sys.argv[1]).resolve()), sys.argv[2] if len(sys.argv) > 2 else None)
