#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os

from assembly import (
    AssemblyInputBundle,
    PackageManifest,
)
from depfile import DepFile
from serialization.serialization import json_load


def collect_packages(aibs, static, bootfs, input_static):
    static_packages = set()
    bootfs_packages = set()
    for aib, _ in aibs:
        for pkg in aib.packages:
            if pkg.set == "base":
                name = pkg.package.removeprefix("packages/base/")
                static_packages.add(name)
            elif pkg.set == "cache":
                name = pkg.package.removeprefix("packages/cache/")
                static_packages.add(name)
            elif pkg.set == "flexible":
                name = pkg.package.removeprefix("packages/flexible/")
                static_packages.add(name)
            elif pkg.set == "bootfs":
                name = pkg.package.removeprefix("packages/bootfs_packages/")
                bootfs_packages.add(name)
        for base_driver in aib.base_drivers:
            name = base_driver.package.removeprefix("packages/base_drivers/")
            static_packages.add(name)

    if input_static:
        for line in input_static.readlines():
            static_packages.add(line.strip())

    for line in sorted(static_packages):
        static.write("?" + line + "\n")
    for line in sorted(bootfs_packages):
        bootfs.write("?" + line + "\n")


def collect_bootfs_files(aibs, bootfs_files, input_bootfs_files):
    files = set()
    deps = []
    for aib, aib_path in aibs:
        if aib.bootfs_files_package:
            manifest_path = os.path.join(aib_path, aib.bootfs_files_package)
            deps.append(manifest_path)
            with open(manifest_path, "r") as f:
                manifest = json_load(PackageManifest, f)
                for blob in manifest.blobs:
                    if blob.path.startswith("meta/"):
                        continue
                    path = blob.path.removeprefix("bootfs/")
                    files.add(path)

    if input_bootfs_files:
        for line in input_bootfs_files.readlines():
            files.add(line.strip())

    for line in sorted(files):
        bootfs_files.write("?" + line + "\n")

    return deps


def collect_kernel_cmdline(aibs, kernel_cmdline):
    cmdline = set()
    for aib, _ in aibs:
        cmdline.update(aib.kernel.args)
        cmdline.update(aib.boot_args)

    for line in sorted(cmdline):
        kernel_cmdline.write("?" + line + "\n")


def main():
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
        "--static-packages-input",
        type=argparse.FileType("r"),
        help="Optional static packages to merge in",
    )
    parser.add_argument(
        "--bootfs-files-input",
        type=argparse.FileType("r"),
        help="Optional list of bootfs packages to merge in",
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
    aibs = []
    for aib in args.assembly_input_bundles:
        aib_path = os.path.dirname(aib.name)
        aibs.append((json_load(AssemblyInputBundle, aib), aib_path))

    collect_packages(
        aibs,
        args.static_packages_output,
        args.bootfs_packages_output,
        args.static_packages_input,
    )
    collect_kernel_cmdline(aibs, args.kernel_cmdline_output)
    deps = collect_bootfs_files(
        aibs, args.bootfs_files_output, args.bootfs_files_input
    )

    if args.depfile:
        DepFile.from_deps(args.static_packages_output.name, deps).write_to(
            args.depfile
        )
