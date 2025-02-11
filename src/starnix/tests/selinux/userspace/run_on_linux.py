#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Runs SEStarnix userspace tests on Linux in qemu."""

import argparse
import fnmatch
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

SUCCESS_RE = re.compile("^TEST SUCCESS$", re.MULTILINE)


def parse_manifest(path, output_dir):
    """Returns a mapping of package file to file path from the package manifest at path."""
    files = {}
    for l in path.read_text().splitlines():
        dest, origin = l.strip().split("=", 1)
        files[dest] = output_dir / origin
    return files


def build_initrd(work_dir, fuchsia_dir):
    """Builds an initrd containing the tests and associated files.

    Args:
      work_dir: the temporary dir we are working in.
      fuchsia_dir: the root of the Fuchsia checkout.

    Returns:
      A pair of the path to the initrd, and the list of tests found.
    """

    output_dir = pathlib.Path(
        subprocess.check_output(
            ["fx", "get-build-dir"], cwd=fuchsia_dir, text=True
        ).strip()
    )
    container_manifest = (
        output_dir
        / "obj/src/starnix/tests/selinux/userspace/sestarnix_userspace_test_container_manifest"
    )
    tests_manifest = (
        output_dir
        / "obj/src/starnix/tests/selinux/userspace/sestarnix_userspace_tests_manifest"
    )
    files = parse_manifest(container_manifest, output_dir)
    files.update(parse_manifest(tests_manifest, output_dir))

    initrd_dir = work_dir / "initrd"
    initrd_dir.mkdir()

    tests = []
    for package_path, output_path in files.items():
        if package_path.startswith(
            "data/tests/"
        ) and not package_path.startswith("data/tests/expectations/"):
            tests.append(package_path.removeprefix("data/tests/"))
        if package_path == "data/bin/init_for_linux":
            dest_path = initrd_dir / "init"
        elif package_path.startswith("data/lib/"):
            dest_path = initrd_dir / package_path.removeprefix("data/")
        else:
            dest_path = initrd_dir / package_path
        dest_path.parent.mkdir(exist_ok=True, parents=True)
        shutil.copy(output_path, initrd_dir / dest_path)

    subprocess.run(
        "find . | cpio --quiet -H newc -o | gzip -9 -n > ../initrd.img",
        shell=True,
        check=True,
        cwd=initrd_dir,
    )
    return (work_dir / "initrd.img"), tests


def run_test(work_dir, test_name, kernel_path, initrd_path):
    """Runs a test, returns success or failure."""

    print(f"Running {test_name}")
    result = subprocess.run(
        [
            "qemu-system-x86_64",
            "-kernel",
            kernel_path,
            "-initrd",
            initrd_path,
            "-no-reboot",
            "-display",
            "none",
            "-serial",
            "stdio",
            "-enable-kvm",
            "-append",
            "console=ttyS0 security=selinux debug=all panic=-1 -- data/tests/"
            + test_name,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=True,  # This indicates test runner failures
    )
    (work_dir / "output").mkdir(exist_ok=True, parents=True)
    (work_dir / "output" / (test_name + ".log")).write_text(result.stdout)
    if SUCCESS_RE.search(result.stdout):
        print("... OK")
        return True
    else:
        print("... FAILED")
        print("Output tail:")
        print(*result.stdout.splitlines()[-30:], sep="\n")
        print(f"End of output ({test_name})")
        return False


def main():
    parser = argparse.ArgumentParser(
        "run_on_linux.py",
    )
    parser.add_argument(
        "--test_filter", type=str, default="*", help="Test filter."
    )
    parser.add_argument(
        "--preserve_work_dir",
        help="Keep the work directory on exit.",
        action="store_true",
    )
    args = parser.parse_args()

    work_dir = pathlib.Path(tempfile.mkdtemp())
    try:
        build_and_run_tests(work_dir, args)
    finally:
        if args.preserve_work_dir:
            print(f"Workdir preserved at {work_dir}")
        else:
            shutil.rmtree(work_dir)


def build_and_run_tests(work_dir, args):
    if "FUCHSIA_DIR" not in os.environ:
        print("FUCHSIA_DIR is not set", file=sys.stderr)
        sys.exit(1)
    fuchsia_dir = pathlib.Path(os.environ["FUCHSIA_DIR"])
    kernel_path = fuchsia_dir / "local/vmlinuz"
    if not kernel_path.is_file():
        print(f"No kernel found at {kernel_path}", file=sys.stderr)

    print("Re-building tests...")
    subprocess.run(
        ["fx", "build", "sestarnix_userspace_tests"],
        check=True,
        cwd=fuchsia_dir,
    )

    initrd_path, tests = build_initrd(work_dir, fuchsia_dir)
    matched_tests = fnmatch.filter(tests, args.test_filter)
    print(f"Matched {len(matched_tests)} tests.")
    failed_tests = []
    for test_name in sorted(matched_tests):
        if not run_test(work_dir, test_name, kernel_path, initrd_path):
            failed_tests.append(test_name)
    if failed_tests:
        print(f"Failed tests:")
        for test_name in failed_tests:
            print("  " + test_name)
        sys.exit(1)


if __name__ == "__main__":
    main()
