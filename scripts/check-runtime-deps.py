#!/usr/bin/env python3
"""Check that the Windows programs do not need the separately installed C/C++ runtime (Buildoffice).

The programs (vault-mcp.exe, vault_ffi.dll and the app's vault_app.exe) are built with the C/C++
runtime linked in statically, so that they start on a PC where the runtime package is not installed.
This script reads the import table and the delay-load import table of every .exe and .dll under the
given paths and reports the DLL names of that runtime package (the "Visual C++ Redistributable"):

    VCRUNTIME*, MSVCP*, CONCRT*, VCOMP*, VCCORLIB*, MSVCR<digit>* (MSVCR100.dll and so on), MSVCRTD*,
    MFC<digit>* / MFCM<digit>* (MFC140.dll and so on)

and the DLL names of the dynamically linked universal CRT, which the programs do not use now
(they are fully static; this keeps that state, so that a change of a build setting shows up):

    api-ms-win-crt-*, ucrtbase*.dll (this includes the debug ucrtbased.dll)

MSVCRT.dll is a part of Windows and is not reported (nor are the other system DLLs such as
api-ms-win-core-*, MFCORE.dll or MFPLAT.dll).

The files are only read (nothing is loaded or run). Only the Python standard library is used.
A file that cannot be read as a PE file is reported too (it is not skipped).

Exit code 1 if such a DLL is found, if a file cannot be read, or if no .exe or .dll is found; 0 otherwise.

Usage: python3 scripts/check-runtime-deps.py [--list] PATH...
  --list   also print every DLL name that each file imports
"""

import argparse
import re
import struct
import sys
from pathlib import Path

# The DLL names of the runtime package that is installed separately (matched from the start of the name,
# ignoring the case). "MSVCR" and "MFC" are followed by a digit: MSVCRT.dll (MSVCR + T) is a part of
# Windows (only its debug form MSVCRTD.dll is not), and MFCORE.dll (Media Foundation) is a part of Windows too.
RUNTIME_RE = re.compile(r"(VCRUNTIME|MSVCP|CONCRT|VCOMP|VCCORLIB|MSVCR[0-9]|MSVCRTD|MFCM?[0-9])", re.IGNORECASE)
# The dynamically linked universal CRT (the programs are linked with it statically now).
DYNAMIC_CRT_RE = re.compile(r"(API-MS-WIN-CRT-|UCRTBASE)", re.IGNORECASE)
PE_SUFFIXES = (".exe", ".dll")

# Limits against a broken file (a real program has far fewer entries and shorter names).
MAX_DESCRIPTORS = 4096
MAX_NAME = 512

IMPORT_DIRECTORY = 1
DELAY_IMPORT_DIRECTORY = 13


class PeError(Exception):
    """The file is not a PE file that this script can read."""


def _unpack(fmt, data, off):
    size = struct.calcsize(fmt)
    if off < 0 or off + size > len(data):
        raise PeError(f"truncated at offset {off:#x}")
    return struct.unpack_from(fmt, data, off)[0]


def _sections(data, off, count):
    """[(virtual address, virtual size, raw pointer, raw size)] of the section table."""
    out = []
    for i in range(count):
        s = off + 40 * i
        vsize = _unpack("<I", data, s + 8)
        va = _unpack("<I", data, s + 12)
        raw_size = _unpack("<I", data, s + 16)
        raw_ptr = _unpack("<I", data, s + 20)
        out.append((va, max(vsize, raw_size), raw_ptr, raw_size))
    return out


def _offset(rva, sections):
    for va, size, raw_ptr, raw_size in sections:
        if va <= rva < va + size:
            delta = rva - va
            if delta >= raw_size:
                raise PeError(f"address {rva:#x} has no data in the file")
            return raw_ptr + delta
    raise PeError(f"address {rva:#x} is not in any section")


def _name(data, off):
    end = data.find(b"\0", off, off + MAX_NAME)
    if off >= len(data) or end < 0:
        raise PeError(f"DLL name at offset {off:#x} is not terminated")
    try:
        return data[off:end].decode("ascii")
    except UnicodeDecodeError:
        raise PeError(f"DLL name at offset {off:#x} is not ASCII") from None


def imported_dlls(data):
    """[(kind, DLL name)] of a PE file; kind is "import" or "delay"."""
    if data[:2] != b"MZ":
        raise PeError("no MZ signature")
    pe = _unpack("<I", data, 0x3C)
    if data[pe : pe + 4] != b"PE\0\0":
        raise PeError("no PE signature")
    coff = pe + 4
    section_count = _unpack("<H", data, coff + 2)
    optional_size = _unpack("<H", data, coff + 16)
    opt = coff + 20
    magic = _unpack("<H", data, opt)
    if magic == 0x10B:  # PE32
        image_base = _unpack("<I", data, opt + 28)
        dirs = opt + 96
    elif magic == 0x20B:  # PE32+
        image_base = _unpack("<Q", data, opt + 24)
        dirs = opt + 112
    else:
        raise PeError(f"unknown optional header magic {magic:#x}")
    dir_count = _unpack("<I", data, dirs - 4)
    sections = _sections(data, opt + optional_size, section_count)

    def directory(index):
        if index >= dir_count:
            return 0
        return _unpack("<I", data, dirs + 8 * index)

    found = []
    rva = directory(IMPORT_DIRECTORY)
    if rva:
        base = _offset(rva, sections)
        for i in range(MAX_DESCRIPTORS):
            d = base + 20 * i
            name_rva = _unpack("<I", data, d + 12)
            first_thunk = _unpack("<I", data, d + 16)
            # The loader stops at the first descriptor without a name or without a thunk table.
            if name_rva == 0 or first_thunk == 0:
                break
            found.append(("import", _name(data, _offset(name_rva, sections))))
        else:
            raise PeError("too many import descriptors")
    rva = directory(DELAY_IMPORT_DIRECTORY)
    if rva:
        base = _offset(rva, sections)
        for i in range(MAX_DESCRIPTORS):
            d = base + 32 * i
            attributes = _unpack("<I", data, d)
            name_rva = _unpack("<I", data, d + 4)
            if name_rva == 0:
                break
            if attributes & 1 == 0:
                # The old form holds virtual addresses instead of relative ones.
                name_rva -= image_base
            found.append(("delay", _name(data, _offset(name_rva, sections))))
        else:
            raise PeError("too many delay-load import descriptors")
    return found


def runtime_dlls(imports):
    """The (kind, name) pairs of the separately installed runtime or of the dynamically linked universal CRT."""
    return [(kind, name) for kind, name in imports if RUNTIME_RE.match(name) or DYNAMIC_CRT_RE.match(name)]


def pe_files(arg):
    base = Path(arg)
    if base.is_file():
        return [(base, base.name)]
    if not base.is_dir():
        raise FileNotFoundError(arg)
    return [
        (p, p.relative_to(base).as_posix())
        for p in sorted(base.rglob("*"))
        if p.is_file() and p.suffix.lower() in PE_SUFFIXES
    ]


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true", help="also print every imported DLL name")
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args()
    count = 0
    bad = 0
    for arg in args.paths:
        try:
            files = pe_files(arg)
        except FileNotFoundError:
            print(f"{arg}: not found")
            bad += 1
            continue
        for path, rel in files:
            count += 1
            try:
                imports = imported_dlls(path.read_bytes())
            except PeError as e:
                print(f"{rel}: cannot read as a PE file ({e})")
                bad += 1
                continue
            if args.list:
                shown = ", ".join(name if kind == "import" else f"{name} (delay)" for kind, name in imports)
                print(f"{rel}: imports {shown or '(nothing)'}")
            hits = runtime_dlls(imports)
            if hits:
                bad += 1
                shown = ", ".join(name if kind == "import" else f"{name} (delay)" for kind, name in hits)
                print(f"{rel}: needs the separately installed C/C++ runtime or loads the C runtime dynamically: {shown}")
    if count == 0:
        print("no .exe or .dll found")
        return 1
    print(f"checked {count} files: {bad} file(s) with findings")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
