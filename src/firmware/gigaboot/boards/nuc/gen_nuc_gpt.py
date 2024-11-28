#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generate a GPT image for bootstrapping NUC"""

import argparse
import pathlib
import subprocess
import sys
import tempfile

_SCRIPT_DIR = pathlib.Path(__file__).parent.resolve()
_FUCHSIA_ROOT = _SCRIPT_DIR.parent.parent.parent.parent.parent
_CGPT = _FUCHSIA_ROOT / "prebuilt" / "tools" / "cgpt" / "linux-x64" / "cgpt"

_SZ_MB = 1024 * 1024
_BLOCK_SIZE = 512
_FIRST = 34  # MBR + GPT header + GPT entries

# Partition information taken mostly from src/storage/lib/paver/x64.cc
PARTITIONS = [
    ("fuchsia-esp", "C12A7328-F81F-11D2-BA4B-00A0C93EC93B", 16 * _SZ_MB),
    ("zircon-a", "DE30CC86-1F4A-4A31-93C4-66F147D33E05", 128 * _SZ_MB),
    ("zircon-b", "23CC04DF-C278-4CE7-8471-897D1A4BCDF7", 128 * _SZ_MB),
    ("zircon-r", "A0E5CF57-2DEF-46BE-A80C-A2067C37CD49", 196 * _SZ_MB),
    ("vbmeta_a", "A13B4D9A-EC5F-11E8-97D8-6C3BE52705BF", 64 * 1024),
    ("vbmeta_b", "A288ABF2-EC5F-11E8-97D8-6C3BE52705BF", 64 * 1024),
    ("vbmeta_r", "6A2460C3-CD11-4E8B-80A8-12CCE268ED0A", 64 * 1024),
    ("durable_boot", "1D75395D-F2C6-476B-A8B7-45CC1C97B476", 4 * 1024),
    ("fvm", "41D0E340-57E3-954E-8C1E-17ECAC44CFF5", 56 * 1024 * _SZ_MB),
]


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("out", help="path to output file")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    with tempfile.TemporaryDirectory() as tempdir:
        tempdir = pathlib.Path(tempdir)
        disk_file = tempdir / "disk.img"
        total = sum([ele[2] for ele in PARTITIONS])
        total += (1 + 33 * 2) * _BLOCK_SIZE
        with open(disk_file, "wb") as f:
            f.truncate(total)
        subprocess.run([_CGPT, "create", f"{disk_file}"], check=True)
        subprocess.run([_CGPT, "boot", "-p", f"{disk_file}"], check=True)
        off = _FIRST
        for name, type, sz in PARTITIONS:
            sz = sz // _BLOCK_SIZE
            subprocess.run(
                [
                    _CGPT,
                    "add",
                    "-b",
                    f"{off}",
                    "-s",
                    f"{sz}",
                    "-t",
                    type,
                    "-l",
                    f"{name}",
                    f"{disk_file}",
                ],
                check=True,
            )
            off += sz
        # Extracts the primary GPT
        pathlib.Path(args.out).write_bytes(
            disk_file.read_bytes()[: _FIRST * _BLOCK_SIZE]
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
