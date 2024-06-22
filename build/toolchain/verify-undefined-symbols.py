#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--ignore-unresolved-symbol",
        action="append",
        help="Expected undefined symbol name (may be repeated)",
        default=[],
    )
    parser.add_argument(
        "--ignore-all-unresolved-symbols",
        action="store_true",
        help="Allow all undefined symbol names",
        default=False,
    )
    parser.add_argument(
        "--init-array",
        action="store_true",
        help="Allow SHT_INIT_ARRAY sections",
        default=False,
    )
    parser.add_argument(
        "--fini-array",
        action="store_true",
        help="Allow SHT_FINI_ARRAY sections",
        default=False,
    )
    parser.add_argument(
        "--depfile",
        type=argparse.FileType("w"),
        required=True,
    )
    parser.add_argument(
        "--stamp",
        type=argparse.FileType("w"),
        help="Stamp file written on success",
        required=True,
    )
    parser.add_argument(
        "--rspfile",
        type=argparse.FileType("r"),
        help="Response file listing name of ET_REL file to check",
        required=True,
    )
    parser.add_argument("readelf", help="llvm-readelf binary", nargs=1)
    args = parser.parse_args()

    [file] = [line.strip() for line in args.rspfile.readlines()]
    args.rspfile.close()

    args.depfile.write(f"{args.stamp.name}: {file}\n")
    args.depfile.close()

    data = json.loads(
        subprocess.check_output(
            [
                args.readelf[0],
                "--elf-output-style=JSON",
                "--sections",
                "--symbols",
                file,
            ]
        )
    )

    sections = [entry["Section"] for entry in data[0]["Sections"]]
    symbols = [entry["Symbol"] for entry in data[0]["Symbols"]]

    # The section with index 0 is always the null section header, but
    # llvm-readelf includes it just like any other.
    assert sections[0]["Name"]["Value"] == 0
    assert sections[0]["Type"]["Value"] == 0
    sections = sections[1:]

    init_sections = set(
        [
            section["Name"]["Name"]
            for section in sections
            if section["Type"]["Name"] == "SHT_INIT_ARRAY"
        ]
    )

    fini_sections = set(
        [
            section["Name"]["Name"]
            for section in sections
            if section["Type"]["Name"] == "SHT_FINI_ARRAY"
        ]
    )

    # The symbol with index 0 is always the null symbol, but llvm-readelf
    # includes it just like any other.
    assert symbols[0]["Name"]["Value"] == 0
    assert symbols[0]["Section"]["Value"] == 0
    symbols = symbols[1:]

    ok = True

    if not args.ignore_all_unresolved_symbols:
        allowed_symbols = set(args.ignore_unresolved_symbol)
        undefined_symbols = set(
            [
                symbol["Name"]["Name"]
                for symbol in symbols
                if symbol["Section"]["Value"] == 0
            ]
        )
        if not undefined_symbols <= allowed_symbols:
            ok = False
            sys.stderr.write(f"*** {file} FAILED undefined symbol check ***\n")
            for extra in sorted(undefined_symbols - allowed_symbols):
                sys.stderr.write(f"*** Unexpected reference: {extra}\n")
            sys.stderr.write(
                f"*** Find references with: llvm-objdump -drl {file}\n"
            )

    if init_sections and not args.init_array:
        ok = False
        sys.stderr.write(f"*** {file} FAILED .init_array check ***\n")
        for name in sorted(init_sections):
            sys.stderr.write(f"*** Unexpected SHT_INIT_ARRAY section {name}\n")

    if fini_sections and not args.fini_array:
        ok = False
        sys.stderr.write(f"*** {file} FAILED .fini_array check ***\n")
        for name in sorted(fini_sections):
            sys.stderr.write(f"*** Unexpected SHT_FINI_ARRAY section {name}\n")

    if not ok:
        return 1

    args.stamp.write("OK\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
