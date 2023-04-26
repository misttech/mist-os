# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import pathlib
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(
        description=
        "Wrap configc's package validator and provide in-tree-specific suggestions on failure."
    )
    parser.add_argument(
        "--configc-bin",
        type=pathlib.Path,
        required=True,
        help="Path to configc binary.")
    parser.add_argument(
        "--package",
        type=pathlib.Path,
        required=True,
        help="Path to package manifest.")
    parser.add_argument(
        "--stamp",
        type=pathlib.Path,
        required=True,
        help="Path to stamp file to write when complete.")
    args = parser.parse_args()

    output = subprocess.run(
        [
            args.configc_bin,
            "validate-package",
            args.package,
            "--stamp",
            "/dev/null",
        ])

    if output.returncode != 0:
        print(
            """
Validating structured configuration failed!

If this is a fuchsia_test_package() and you are using
RealmBuilder to provide all values, consider setting
`validate_structured_config=false` on this target to
disable this check.
""")
        sys.exit(output.returncode)

    with open(args.stamp, 'w') as f:
        f.write('')
