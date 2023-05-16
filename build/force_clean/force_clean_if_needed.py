#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Process build "force clean fences". If it detects that the build should be clobbered,
it will run `gn clean`.

A fence is crossed when a machine checks out a different revision and
there is a diff between $BUILD_DIR/.force_clean_fences and the outputs of the main
get_fences.py file and get_fences.py files under vendor/*/.
"""

import argparse
import pathlib
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(
        description="Process build force-clean fences.")
    parser.add_argument(
        "--gn-bin",
        type=pathlib.Path,
        required=True,
        help="Path to prebuilt GN binary.")
    parser.add_argument(
        "--checkout-dir",
        type=pathlib.Path,
        required=True,
        help="Path to $FUCHSIA_DIR.")
    parser.add_argument(
        "--build-dir",
        type=pathlib.Path,
        required=True,
        help="Path to the root build dir, e.g. $FUCHSIA_DIR/out/default.")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    def _log(message):
        if args.verbose:
            print(message)

    # check that inputs are valid
    if not args.gn_bin.is_file():
        raise RuntimeError(f"{args.gn_bin} is not a file")
    if not args.checkout_dir.is_dir():
        raise RuntimeError(f"{args.checkout_dir} is not a directory")
    if not args.build_dir.is_dir():
        raise RuntimeError(f"{args.build_dir} is not a directory")

    # Find the main get_fences.py script and all those under vendor/*/
    get_fences_scripts = []
    subdirs = [args.checkout_dir]
    vendor_dir = args.checkout_dir / "vendor"
    if vendor_dir.exists():
        subdirs.extend(vendor_dir.iterdir())
    for subdir in subdirs:
        get_fences_script = subdir / "build" / "force_clean" / "get_fences.py"
        if get_fences_script.exists():
            get_fences_scripts.append(get_fences_script)

    # execute fences scripts using the same interpreter as us
    current_fences = []
    for script in sorted(get_fences_scripts):
        _log(f"generating clean-build fences from {script}")
        current_fences.append(
            subprocess.run(
                [sys.executable, script],
                stdout=subprocess.PIPE,
                text=True,
                check=True).stdout)

    current_fences = "\n".join(current_fences)

    existing_fences = None
    existing_fences_path = args.build_dir / ".force_clean_fences"
    if existing_fences_path.exists():
        _log("reading existing build dir's force-clean fences")
        with open(existing_fences_path, "r") as f:
            existing_fences = f.read()

    def _write_fences():
        _log(
            f"writing new fences:\n=============\n{current_fences}\n============="
        )
        with open(existing_fences_path, "w") as f:
            f.write(current_fences)

    # clobber if needed
    if existing_fences == None:
        _log("no fences file found, assuming nothing to clean")
        _write_fences()

    elif existing_fences != current_fences:
        _log(f"new fences found, clobbering build...")
        subprocess.run([args.gn_bin, "clean", args.build_dir])
        _write_fences()
    else:
        _log("force_clean fences up-to-date, not clobbering")

    return 0


if __name__ == '__main__':
    sys.exit(main())
