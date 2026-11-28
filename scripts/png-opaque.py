#!/usr/bin/env python3
"""Rewrite an opaque 8-bit RGBA PNG as RGB.

Google Play takes screenshots and feature graphics only without an alpha
channel, while slint-viewer and Chromium may write one. Refuses images with
any transparent pixel, which would need a background chosen first.
"""

import argparse
from pathlib import Path
import struct
import sys
import zlib

SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunks(data):
    position = len(SIGNATURE)
    while position < len(data):
        length, kind = struct.unpack(">I4s", data[position:position + 8])
        yield kind, data[position + 8:position + 8 + length]
        position += 12 + length


def rows(data, width, height, channels):
    raw = zlib.decompress(b"".join(body for kind, body in chunks(data) if kind == b"IDAT"))
    stride = width * channels
    previous = bytearray(stride)
    offset = 0
    for _ in range(height):
        kind = raw[offset]
        line = bytearray(raw[offset + 1:offset + 1 + stride])
        offset += 1 + stride
        for x in range(stride):
            left = line[x - channels] if x >= channels else 0
            up = previous[x]
            corner = previous[x - channels] if x >= channels else 0
            if kind == 1:
                line[x] = (line[x] + left) & 255
            elif kind == 2:
                line[x] = (line[x] + up) & 255
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 255
            elif kind == 4:
                estimate = left + up - corner
                distances = (abs(estimate - left), abs(estimate - up), abs(estimate - corner))
                line[x] = (line[x] + (left if distances[0] <= distances[1] and distances[0] <= distances[2]
                                      else up if distances[1] <= distances[2] else corner)) & 255
            elif kind != 0:
                raise ValueError(f"unknown PNG filter {kind}")
        yield bytes(line)
        previous = line


def opaque_rgb(source: Path, destination: Path) -> None:
    data = source.read_bytes()
    if data[:8] != SIGNATURE or data[12:16] != b"IHDR":
        raise ValueError(f"{source} is not a PNG")
    width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", data[16:29])
    if depth != 8 or color not in (2, 6) or interlace:
        raise ValueError(f"{source} must be a non-interlaced 8-bit RGB or RGBA PNG")
    channels = 4 if color == 6 else 3
    encoded = bytearray()
    above = bytes(width * 3)
    for line in rows(data, width, height, channels):
        if channels == 4:
            if any(line[x] != 255 for x in range(3, len(line), 4)):
                raise ValueError(f"{source} has transparent pixels")
            line = bytes(byte for index, byte in enumerate(line) if index % 4 != 3)
        # The Up filter compresses screenshots' flat areas well.
        encoded.append(2)
        encoded.extend((value - prior) & 255 for value, prior in zip(line, above))
        above = line

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    destination.write_bytes(SIGNATURE
                            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                            + chunk(b"IDAT", zlib.compress(bytes(encoded), 9))
                            + chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        opaque_rgb(args.source, args.destination)
    except (OSError, ValueError, zlib.error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
