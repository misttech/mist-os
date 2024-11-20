#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
from typing import TextIO

from assembly import AssemblyInputBundle, PackageManifest
from depfile import DepFile
from serialization.serialization import json_load


def collect_aib_artifacts(aib: AssemblyInputBundle, aib_path: str) -> DepFile:
    static_packages = set()
    bootfs_packages = set()
    for pkg in aib.packages:
        if pkg.set == "base":
            name = pkg.package.removeprefix("packages/")
            static_packages.add(name)
        elif pkg.set == "cache":
            name = pkg.package.removeprefix("packages/")
            static_packages.add(name)
        elif pkg.set == "flexible":
            name = pkg.package.removeprefix("packages/")
            static_packages.add(name)
        elif pkg.set == "bootfs":
            name = pkg.package.removeprefix("packages/")
            bootfs_packages.add(name)
    for base_driver in aib.base_drivers:
        name = base_driver.package.removeprefix("packages/")
        static_packages.add(name)
    for boot_driver in aib.boot_drivers:
        name = boot_driver.package.removeprefix("packages/")
        bootfs_packages.add(name)

    bootfs_files = set()
    deps = []
    if aib.bootfs_files_package:
        manifest_path = os.path.join(aib_path, aib.bootfs_files_package)
        deps.append(manifest_path)
        with open(manifest_path, "r") as f:
            manifest = json_load(PackageManifest, f)
            for blob in manifest.blobs:
                if blob.path.startswith("meta/"):
                    continue
                path = blob.path.removeprefix("bootfs/")
                bootfs_files.add(path)

    cmdline = set()
    cmdline.update(aib.kernel.args)
    cmdline.update(aib.boot_args)

    return (static_packages, bootfs_packages, bootfs_files, cmdline, deps)


class Golden:
    def __init__(self) -> None:
        self.lines: set[str] = set()

    def add_optional(self, names: set[str]) -> None:
        for name in names:
            name = name.strip()
            if name not in self.lines:
                self.lines.add("?" + name)

    def add_required(self, names: set[str]) -> None:
        for name in names:
            name = name.strip()
            optional = "?" + name
            if optional in self.lines:
                self.lines.remove(optional)
            self.lines.add(name)

    def write(self, output: TextIO) -> None:
        lines = sorted(self.lines, key=lambda s: s.removeprefix("?"))
        for line in lines:
            output.write(line + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Tool that parses AIBs and generates relevant scrutiny configs for the platform"
    )
    parser.add_argument(
        "--assembly-input-bundles",
        nargs="+",
        type=argparse.FileType("r"),
        help="Path to an assembly input bundle config to search for artifacts",
    )
    parser.add_argument(
        "--required-assembly-input-bundles",
        nargs="+",
        type=argparse.FileType("r"),
        help="Path to an assembly input bundle config to search for artifacts",
    )
    parser.add_argument(
        "--static-packages-input",
        type=argparse.FileType("r"),
        help="Optional static packages to merge in",
    )
    parser.add_argument(
        "--bootfs-packages-input",
        type=argparse.FileType("r"),
        help="Optional bootfs packages to merge in",
    )
    parser.add_argument(
        "--bootfs-files-input",
        type=argparse.FileType("r"),
        help="Optional list of bootfs packages to merge in",
    )
    parser.add_argument(
        "--kernel-args-input",
        type=argparse.FileType("r"),
        help="Optional list of kernel arguments to merge in",
    )
    parser.add_argument(
        "--static-packages-output",
        required=True,
        type=argparse.FileType("w"),
        help="Path to the output list of static packages",
    )
    parser.add_argument(
        "--bootfs-packages-output",
        required=True,
        type=argparse.FileType("w"),
        help="Path to the output list of bootfs packages",
    )
    parser.add_argument(
        "--bootfs-files-output",
        required=True,
        type=argparse.FileType("w"),
        help="Path to the output list of bootfs files",
    )
    parser.add_argument(
        "--kernel-cmdline-output",
        required=True,
        type=argparse.FileType("w"),
        help="Path to the output list of kernel cmdlines",
    )
    parser.add_argument(
        "--depfile",
        type=argparse.FileType("w"),
    )

    args = parser.parse_args()

    deps = []
    static_packages = Golden()
    bootfs_packages = Golden()
    bootfs_files = Golden()
    kernel_cmdline = Golden()

    for file in args.assembly_input_bundles:
        aib_path = os.path.dirname(file.name)
        aib = json_load(AssemblyInputBundle, file)

        (sp, bp, bf, kc, d) = collect_aib_artifacts(aib, aib_path)
        deps.extend(d)
        static_packages.add_optional(sp)
        bootfs_packages.add_optional(bp)
        bootfs_files.add_optional(bf)
        kernel_cmdline.add_optional(kc)

    for file in args.required_assembly_input_bundles:
        aib_path = os.path.dirname(file.name)
        aib = json_load(AssemblyInputBundle, file)

        (sp, bp, bf, kc, d) = collect_aib_artifacts(aib, aib_path)
        deps.extend(d)
        static_packages.add_required(sp)
        bootfs_packages.add_required(bp)
        bootfs_files.add_required(bf)
        kernel_cmdline.add_required(kc)

    # Merge in the optional input goldens.
    if args.static_packages_input:
        static_packages.add_optional(args.static_packages_input.readlines())
    if args.bootfs_packages_input:
        bootfs_packages.add_optional(args.bootfs_packages_input.readlines())
    if args.bootfs_files_input:
        bootfs_files.add_optional(args.bootfs_files_input.readlines())
    if args.kernel_args_input:
        kernel_cmdline.add_optional(args.kernel_args_input.readlines())

    with args.static_packages_output as static_packages_output:
        static_packages.write(static_packages_output)

    with args.bootfs_packages_output as bootfs_packages_output:
        bootfs_packages.write(bootfs_packages_output)

    with args.bootfs_files_output as bootfs_files_output:
        bootfs_files.write(bootfs_files_output)

    with args.kernel_cmdline_output as kernel_cmdline_output:
        kernel_cmdline.write(kernel_cmdline_output)

    if args.depfile:
        with args.depfile as depfile:
            DepFile.from_deps(args.static_packages_output.name, deps).write_to(
                depfile
            )

    return 0
