#!/usr/bin/env python3
"""Name and check the ELF files of a Linux package.

`arch EXECUTABLE` prints the package architecture names of the executable
being packaged: its machine name (AppImage and RPM), Debian architecture and
PDFium archive, for example `aarch64 arm64 linux-arm64`. Packages describe the
executable they contain rather than the build machine, because a 64-bit
kernel can run a 32-bit userland.

`verify PATH...` checks every ELF file at or below the paths. They must all be
built for one machine, and every load segment must be aligned for the largest
page size that machine's kernels use. Arm64 kernels use 4, 16 or 64 KiB pages
(the Raspberry Pi 5 kernel uses 16 KiB), and they cannot load a segment that
is aligned only for 4 KiB pages.
"""

import argparse
import os
import struct
from pathlib import Path

PT_LOAD = 1

# ELF e_machine: machine name, Debian architecture, PDFium archive and the
# largest page size its Linux kernels use.
MACHINES = {
    62: ("x86_64", "amd64", "linux-x64", 4096),
    183: ("aarch64", "arm64", "linux-arm64", 65536),
}


def read_elf(path):
    """Return the machine and load segments of an ELF file, or None for other files."""
    with open(path, "rb") as stream:
        header = stream.read(64)
        if header[:4] != b"\x7fELF":
            return None
        if len(header) < 64 or header[4:6] != b"\x02\x01":
            raise ValueError(f"{path}: expected a little-endian 64-bit ELF file")
        machine = struct.unpack_from("<H", header, 18)[0]
        table_offset = struct.unpack_from("<Q", header, 32)[0]
        entry_size, count = struct.unpack_from("<HH", header, 54)
        if entry_size < 56:
            raise ValueError(f"{path}: invalid ELF program headers")
        stream.seek(table_offset)
        table = stream.read(entry_size * count)
    if len(table) != entry_size * count:
        raise ValueError(f"{path}: truncated ELF program headers")
    loads = []
    for entry in range(0, len(table), entry_size):
        if struct.unpack_from("<I", table, entry)[0] == PT_LOAD:
            offset, address = struct.unpack_from("<QQ", table, entry + 8)
            alignment = struct.unpack_from("<Q", table, entry + 48)[0]
            loads.append((offset, address, alignment))
    return machine, loads


def machine_names(machine, path):
    if machine not in MACHINES:
        raise ValueError(f"{path}: unsupported ELF machine {machine}")
    return MACHINES[machine]


def architecture(path):
    elf = read_elf(path)
    if elf is None:
        raise ValueError(f"{path} is not an ELF file")
    return machine_names(elf[0], path)[:3]


def elf_files(paths):
    for path in map(Path, paths):
        if not path.is_dir():
            # Name the problem when an expected executable is missing or broken.
            elf = read_elf(path)
            if elf is None:
                raise ValueError(f"{path} is not an ELF file")
            yield path, elf
            continue
        for directory, _, names in sorted(os.walk(path)):
            for name in sorted(names):
                candidate = Path(directory, name)
                # Symlinks name files that the walk already visits.
                if candidate.is_symlink() or not candidate.is_file():
                    continue
                if (elf := read_elf(candidate)) is not None:
                    yield candidate, elf


def verify(paths):
    """Check every ELF file below the paths; return the machine names and file count."""
    first = None
    count = 0
    for path, (machine, loads) in elf_files(paths):
        name, _, _, page_size = machine_names(machine, path)
        if first is None:
            first = (machine, path)
        elif machine != first[0]:
            raise ValueError(f"{path} is built for {name}, but {first[1]} for {MACHINES[first[0]][0]}")
        if not loads:
            raise ValueError(f"{path}: missing ELF load segments")
        for offset, address, alignment in loads:
            if alignment < page_size or offset % page_size != address % page_size:
                raise ValueError(
                    f"{path}: a load segment cannot be mapped with the "
                    f"{page_size // 1024} KiB pages that {name} kernels can use"
                )
        count += 1
    if first is None:
        raise ValueError("no ELF files found in " + ", ".join(map(str, paths)))
    return MACHINES[first[0]], count


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)
    names = commands.add_parser("arch", help="print the package architecture names")
    names.add_argument("executable", type=Path)
    check = commands.add_parser("verify", help="check machines and page alignment")
    check.add_argument("paths", nargs="+", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "arch":
            print(" ".join(architecture(args.executable)))
        else:
            (machine, _, _, page_size), count = verify(args.paths)
            files = "file" if count == 1 else "files"
            print(f"Checked {count} {machine} ELF {files}: load segments fit {page_size // 1024} KiB pages")
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
