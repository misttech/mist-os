#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Updates the Fuchsia platform version.
"""

import argparse
import json
import os
import re
import secrets
import shutil
import sys

from pathlib import Path


def update_fidl_compatibility_doc(
    fuchsia_api_level: int, fidl_compatiblity_doc_path: str
) -> bool:
    """Updates fidl_api_compatibility_testing.md given the in-development API level."""
    try:
        with open(fidl_compatiblity_doc_path, "r+") as f:
            old_content = f.read()
            new_content = re.sub(
                r"\{% set in_development_api_level = \d+ %\}",
                f"{{% set in_development_api_level = {fuchsia_api_level} %}}",
                old_content,
            )
            f.seek(0)
            f.write(new_content)
            f.truncate()
        return True
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'.
Did you run this script from the root of the source tree?""".format(
                path=fidl_compatiblity_doc_path
            ),
            file=sys.stderr,
        )
        return False


def generate_random_abi_revision(already_used: set[int]) -> int:
    """Generates a random 64-bit ABI revision, avoiding values in
    `already_used` and other reserved ranges."""
    while True:
        # Reserve values in the top 1/256th of the space.
        candidate = secrets.randbelow(0xF000_0000_0000_0000)

        # If the ABI revision has already been used, discard it and go buy a
        # lottery ticket.
        if candidate not in already_used:
            return candidate


def update_version_history(
    fuchsia_api_level: int, version_history_path: str
) -> bool:
    """Updates version_history.json to include the given Fuchsia API level.

    The ABI revision for this API level is set to a new random value that has not
    been used before.
    """
    try:
        with open(version_history_path, "r+") as f:
            version_history = json.load(f)
            versions = version_history["data"]["api_levels"]
            if str(fuchsia_api_level) in versions:
                print(
                    "error: Fuchsia API level {fuchsia_api_level} is already defined.".format(
                        fuchsia_api_level=fuchsia_api_level
                    ),
                    file=sys.stderr,
                )
                return False

            for level, data in versions.items():
                if data["status"] == "in-development":
                    print(
                        f"error: Fuchsia API level {level} is in-development. All API levels must be frozen before bumping.",
                        file=sys.stderr,
                    )
                    return False

            abi_revision = generate_random_abi_revision(
                set(int(v["abi_revision"], 16) for v in versions.values())
            )
            versions[str(fuchsia_api_level)] = dict(
                # Print `abi_revision` in hex, with a leading 0x, with capital
                # letters, padded to 16 chars.
                abi_revision=f"0x{abi_revision:016X}",
                status="in-development",
            )
            f.seek(0)
            json.dump(version_history, f, indent=4)
            f.truncate()
            return True
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'.
Did you run this script from the root of the source tree?""".format(
                path=version_history_path
            ),
            file=sys.stderr,
        )
        return False


def create_owners_file_for_in_development_level(level_dir_path: str) -> None:
    """Creates an OWNERS file in `level_dir_path` that allows a wider set of
    reviewers while the level is in development.
    """
    owners_path = os.path.join(level_dir_path, "OWNERS")

    print(f"Creating {owners_path}")
    with open(owners_path, "w") as f:
        f.write("include /sdk/history/IN_DEVELOPMENT_API_LEVEL_OWNERS\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--new-api-level", type=int, required=True)
    parser.add_argument("--sdk-version-history", required=True)
    parser.add_argument("--fidl-compatibility-doc-path", required=True)
    parser.add_argument("--root-source-dir")
    parser.add_argument("--stamp-file")

    args = parser.parse_args()

    new_level = args.new_api_level

    if not update_version_history(new_level, args.sdk_version_history):
        return 1

    if not update_fidl_compatibility_doc(
        new_level, args.fidl_compatibility_doc_path
    ):
        return 1

    level_dir_path = os.path.join(
        args.root_source_dir, "sdk", "history", str(new_level)
    )

    try:
        os.makedirs(level_dir_path, exist_ok=True)
    except Exception as e:
        print(f"Failed to create directory for new level: {e}")
        return 1

    create_owners_file_for_in_development_level(level_dir_path)

    # TODO(https://fxbug.dev/349622444): Automate this.
    # TODO(https://fxbug.dev/326277078): Move this to the instructions for
    # promoting "NEXT" to a stable API level.
    levels_file = "zircon/system/public/zircon/availability_levels.inc"
    sysroot_api_file = "zircon/public/sysroot/sdk/sysroot.api"
    print(
        f"Add `#define FUCHSIA_INTERNAL_LEVEL_{new_level}_() {new_level}` to `//{levels_file}`, run `fx build zircon/public/sysroot/sdk:sysroot_sdk_verify_api`, and follow the instructions to update `//{sysroot_api_file}`."
    )

    # Before printing, rebase the paths to what a developer would use from `//`.
    history = os.path.relpath(args.sdk_version_history, args.root_source_dir)
    compatibility = os.path.relpath(
        args.fidl_compatibility_doc_path, args.root_source_dir
    )
    level_dir = os.path.relpath(level_dir_path, args.root_source_dir)
    print(
        f"Then run `git add -u {history} {compatibility} {levels_file} {sysroot_api_file} && git add {level_dir}`."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
