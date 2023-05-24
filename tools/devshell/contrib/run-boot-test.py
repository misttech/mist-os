#!/usr/bin/env fuchsia-vendored-python
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import json
import os
import platform
import shlex
import subprocess
import sys

# Special values identifying a PE (i.e., Portable Executable) image, of which
# UEFI executables are examples.
#
# In identifying a UEFI executable, it is insufficient to match based on magic
# alone; ARM Linux boot firmware (e.g., our arm64 boot shim) uses that same
# value.
#
# See https://learn.microsoft.com/en-us/windows/win32/debug/pe-format.
PE_MAGIC = b"MZ"
PE_SIGNATURE = b"PE\0\0"


def is_pe(filepath):
    if not os.path.exists(filepath):
        return False
    with open(filepath, "rb") as f:
        if f.read(2) != PE_MAGIC:
            return False
        f.seek(0x3C)
        signature_offset = int.from_bytes(f.read(4), byteorder="little")
        f.seek(signature_offset)
        return f.read(4) == PE_SIGNATURE


class BootTest(object):

    def __init__(self, images_by_label, test_json, build_dir):
        self.build_dir = build_dir
        test = test_json["test"]

        self.name = test["name"]
        self.label = test["label"]
        self.path = test["path"]

        images = test_json["image_overrides"]

        self.zbi = images_by_label[images["zbi"]] if "zbi" in images else None
        self.qemu_kernel = (
            images_by_label[images["qemu_kernel"]]
            if "qemu_kernel" in images else None)
        self.efi_disk = (
            images_by_label[images["efi_disk"]]
            if "efi_disk" in images else None)

        arch_image = self.qemu_kernel or self.efi_disk or self.zbi or None
        self.arch = arch_image.get("cpu", None) if arch_image else None

    # Enables sorting by name.
    def __lt__(self, other):
        return self.name < other.name

    @staticmethod
    def is_boot_test(test_json):
        return "image_overrides" in test_json

    def is_uefi_boot(self):
        if self.efi_disk:
            return True
        # The QEMU kernel might be a UEFI executable. Look for PE magic.
        if not self.qemu_kernel:
            return False
        kernel = os.path.join(self.build_dir, self.qemu_kernel["path"])
        return is_pe(kernel)

    def print(self, command=None):
        kinds = []
        if self.is_uefi_boot():
            kinds.append("UEFI")
        if self.qemu_kernel:
            kinds.append("QEMU kernel")
        if self.zbi:
            kinds.append("ZBI")
        if self.efi_disk:
            kinds.append("EFI disk")
        print("* %s (%s)" % (self.name, ", ".join(kinds)))
        print("    label: %s" % self.label)
        print("    cpu: %s" % (self.arch or "Unknown!"))
        if self.qemu_kernel:
            print("    qemu kernel: %s" % self.qemu_kernel["path"])
        if self.zbi:
            print("    zbi: %s" % self.zbi["path"])
        if self.efi_disk:
            print("    efi disk: %s" % self.efi_disk["path"])
        if command:
            print("    command: %s" % " ".join(map(shlex.quote, command)))


def error(str):
    RED = "\033[91m"
    END = "\033[0m"
    print(RED + "ERROR: " + str + END)


def warning(str):
    YELLOW = "\033[93m"
    END = "\033[0m"
    print(YELLOW + "WARNING: " + str + END)


def find_bootserver(build_dir):
    host_os = {"Linux": "linux", "Darwin": "mac"}[platform.system()]
    host_cpu = {"x86_64": "x64", "arm64": "arm64"}[platform.machine()]
    with open(os.path.join(build_dir, "tool_paths.json")) as file:
        tool_paths = json.load(file)
    bootservers = [
        os.path.join(build_dir, tool["path"]) for tool in tool_paths if (
            tool["name"] == "bootserver" and tool["cpu"] == host_cpu and
            tool["os"] == host_os)
    ]
    if bootservers:
        return bootservers[0]
    print("Cannot find bootserver for %s-%s" % (host_os, host_cpu))
    sys.exit(1)


EPILOG = """
In order to use this tool, please ensure that your boot test (usually defined
by one of zbi_test(), qemu_kernel_test(), or efi_test()) is in your GN graph. A
way to do this is to add //bundles:boot_tests to your `fx set` invocation.
"""


def main():
    parser = argparse.ArgumentParser(
        prog="fx run-boot-test", description="Run a boot test.", epilog=EPILOG)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--boot", "-b", action="store_true", help="Run via bootserver")
    parser.add_argument(
        "--args",
        "-a",
        metavar="RUNNER-ARG",
        action="append",
        default=[],
        help="Pass RUNNER-ARG to bootserver/fx qemu",
    )
    parser.add_argument(
        "--cmdline",
        "-c",
        metavar="KERNEL-ARGS",
        action="append",
        default=[],
        help="Add kernel command-line arguments.",
    )
    parser.add_argument(
        "name",
        help="Name of the boot test (target) to run",
        nargs="?",
    )
    parser.add_argument(
        "--arch",
        help="CPU architecture to run",
        metavar="ARCH",
        default=os.getenv("FUCHSIA_ARCH"),
    )
    args = parser.parse_args()

    build_dir = os.path.relpath(os.getenv("FUCHSIA_BUILD_DIR"))
    if build_dir is None:
        print("FUCHSIA_BUILD_DIR not set")
        return 1
    if args.arch is None:
        print("FUCHSIA_ARCH not set")
        return 1

    # Construct a map of images by GN label. Boot test metadata records its
    # desired images that way.
    with open(os.path.join(build_dir, "images.json")) as file:
        images = {}
        for image in json.load(file):
            images[image["label"]] = image

    # There can be multiple versions of the same boot test for different host
    # architectures. These will otherwise only differ in metadata name, a
    # difference that `BootTest()` normalizes away.
    with open(os.path.join(build_dir, "tests.json")) as file:
        boot_tests = {}
        for test in json.load(file):
            if BootTest.is_boot_test(test):
                boot_test = BootTest(images, test, build_dir)
                if boot_test.arch == args.arch:
                    boot_tests[boot_test.name] = boot_test

    if not boot_tests:
        warning(
            "no boot tests found. Is //bundles:boot_tests in your GN graph?")
        return 0

    if not args.name:
        for test in sorted(boot_tests.values()):
            test.print()
        return 0

    names = [test.name for test in boot_tests.values()]
    # A cut-off of 0.8 was determined to be good enough in experimenting
    # with input names against "core-tests".
    matching_names = difflib.get_close_matches(args.name, names, cutoff=0.8)
    matches = [boot_tests[name] for name in matching_names]
    if len(matches) == 0:
        error("no boot tests closely matching a name of '%s' found" % args.name)
        return 1
    # The returned matches will be ordered by similarlity; if we have an exact
    # match, always go with that.
    elif len(matches) > 1 and matches[0].name != args.name:
        error(
            "no boot tests closely matching a name of '%s' found. Closest matches:"
            % args.name)
        for test in matches:
            test.print()
        return 1

    test = matches[0]
    if args.boot:
        if test.qemu_kernel:
            print(error("cannot use --boot with QEMU-only test %s" % test.name))
            return 1
        assert test.zbi
        bootserver = find_bootserver(build_dir)
        cmd = [bootserver, "--boot"] + test.zbi["path"] + args.args
    else:
        cmd = ["fx", "qemu", "--arch", args.arch] + args.args

        if test.is_uefi_boot():
            cmd += ["--uefi"]
        if test.qemu_kernel:
            cmd += ["-t", test.qemu_kernel["path"]]
        if test.zbi:
            cmd += ["-z", test.zbi["path"]]
        if test.efi_disk:
            cmd += ["-D", test.efi_disk["path"]]

    for arg in args.cmdline:
        cmd += ["-c", arg]

    if not args.boot:
        # Prevents QEMU from boot-looping, as most boot tests do not have a
        # means of gracefully shutting down.
        cmd += ["--", "-no-reboot"]

    test.print(command=cmd)
    return subprocess.run(cmd, cwd=build_dir).returncode


if __name__ == "__main__":
    sys.exit(main())
