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
                    data["status"] = "supported"

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


def move_owners_file(
    root_source_dir: str, root_build_dir: str, fuchsia_api_level: int
) -> bool:
    """Helper function for copying golden files. It accomplishes the following:
    1. Overrides //sdk/history/OWNERS in //sdk/history/N/ allowing a wider set of reviewers.
    2. Reverts //sdk/history/N-1/  back to using //sdk/history/OWNERS, now that N-1 is a
       supported API level.

    """
    root = _join_path(root_source_dir, "sdk", "history")
    src = _join_path(root, str(fuchsia_api_level - 1), "OWNERS")
    dst = _join_path(root, str(fuchsia_api_level))

    try:
        os.mkdir(dst)
    except Exception as e:
        print(f"os.mkdir({dst}) failed: {e}")
        return False

    try:
        print(f"copying {src} to {dst}")
        shutil.move(src, dst)
    except Exception as e:
        print(f"shutil.move({src}, {dst}) failed: {e}")
        return False
    return True


def copy_compatibility_test_goldens(
    root_build_dir: str, fuchsia_api_level: int
) -> bool:
    """Updates the golden files used for compatibility testing.

    This assumes a clean build with:
      fx set core.x64 --with //sdk:compatibility_testing_goldens

    Any files that can't be copied are logged and must be updated manually.
    """
    goldens_manifest = os.path.join(
        root_build_dir, "compatibility_testing_goldens.json"
    )

    with open(goldens_manifest) as f:
        for entry in json.load(f):
            src = _join_path(root_build_dir, entry["src"])
            dst = _join_path(root_build_dir, entry["dst"])
            try:
                print(f"copying {src} to {dst}")
                shutil.copyfile(src, dst)
            except Exception as e:
                print(f"failed to copy {src} to {dst}: {e}")
                return False
    return True


def _join_path(root_dir: str, *paths: str) -> str:
    """Returns absolute path"""
    return os.path.abspath(os.path.join(root_dir, *paths))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fuchsia-api-level", type=int, required=True)
    parser.add_argument("--sdk-version-history", required=True)
    parser.add_argument("--goldens-manifest", required=True)
    parser.add_argument("--fidl-compatibility-doc-path", required=True)
    parser.add_argument("--update-goldens", type=bool, default=True)
    parser.add_argument("--root-build-dir", default="out/default")
    parser.add_argument("--root-source-dir")
    parser.add_argument("--stamp-file")
    parser.add_argument(
        "--revert-on-error",
        action=argparse.BooleanOptionalAction,
        default=False,
    )

    args = parser.parse_args()

    if not update_version_history(
        args.fuchsia_api_level, args.sdk_version_history
    ):
        return 1

    if not update_fidl_compatibility_doc(
        args.fuchsia_api_level, args.fidl_compatibility_doc_path
    ):
        return 1

    if args.update_goldens:
        if not move_owners_file(
            args.root_source_dir, args.root_build_dir, args.fuchsia_api_level
        ):
            return 1
        if not copy_compatibility_test_goldens(
            args.root_build_dir, args.fuchsia_api_level
        ):
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
