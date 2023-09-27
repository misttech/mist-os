#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import argparse
import json
import os
import re
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--sdk_manifests",
        nargs="+",
        required=True,
        help="The list of paths to public and partner SDK manifests",
    )
    parser.add_argument(
        "--output_path",
        required=True,
        help="The target_gen_dir of the invoking action",
    )

    args = parser.parse_args()

    for manifest in args.sdk_manifests:
        if not os.path.isfile(manifest):
            print(f"Manifest {manifest} does not exist")
            return 1

        with open(manifest) as f:
            try:
                data = json.load(f)
            except json.JSONDecodeError:
                print(f"Unable to load {manifest} as JSON")
                return 1

            atoms = set()
            for atom in data["atoms"]:
                # Remove the toolchain from the end of the label. The toolchain
                # is not necessary for dependency verification and does not
                # appear to change what is included in an SDK.
                atoms.add(re.sub("\(.*\)$", "", atom["gn-label"]))

        output_path = os.path.join(args.output_path, os.path.basename(manifest))
        with open(output_path, "w") as output:
            for atom in sorted(atoms):
                output.write(f"{atom}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
