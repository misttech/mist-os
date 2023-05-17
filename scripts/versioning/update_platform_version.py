#!/usr/bin/env python3
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

# The length of the API compatibility window, in API levels. This is the number
# of stable API levels that will be supported in addition to the one in development.
_API_COMPATIBILITY_WINDOW_SIZE = 2


def update_platform_version(fuchsia_api_level, platform_version_path):
    """Updates platform_version.json to set the in_development_api_level to the given
    Fuchsia API level.
    """
    try:
        with open(platform_version_path, "r+") as f:
            platform_version = json.load(f)
            platform_version["in_development_api_level"] = fuchsia_api_level
            # Generates a list of supported API levels that contains at most
            # _API_COMPATIBILITY_WINDOW_SIZE elements.
            platform_version["supported_fuchsia_api_levels"] = list(
                range(
                    max(fuchsia_api_level - _API_COMPATIBILITY_WINDOW_SIZE, 1),
                    fuchsia_api_level))
            f.seek(0)
            json.dump(platform_version, f)
            f.truncate()
        return True
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'.
Did you run this script from the root of the source tree?""".format(
                path=platform_version_path),
            file=sys.stderr)
        return False


def update_fidl_compatibility_doc(
        fuchsia_api_level, fidl_compatiblity_doc_path):
    """Updates fidl_api_compatibility_testing.md given the in-development API level."""
    try:
        with open(fidl_compatiblity_doc_path, "r+") as f:
            old_content = f.read()
            new_content = re.sub(
                r"\{% set in_development_api_level = \d+ %\}",
                f"{{% set in_development_api_level = {fuchsia_api_level} %}}",
                old_content)
            f.seek(0)
            f.write(new_content)
            f.truncate()
        return True
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'.
Did you run this script from the root of the source tree?""".format(
                path=fidl_compatiblity_doc_path),
            file=sys.stderr)
        return False


def generate_random_abi_revision():
    """Generates a random ABI revision.

    ABI revisions are hex encodings of 64-bit, unsigned integeres.
    """
    return '0x{abi_revision}'.format(abi_revision=secrets.token_hex(8).upper())


def update_version_history(fuchsia_api_level, version_history_path):
    """Updates version_history.json to include the given Fuchsia API level.

    The ABI revision for this API level is set to a new random value that has not
    been used before.
    """
    try:
        with open(version_history_path, "r+") as f:
            version_history = json.load(f)
            versions = version_history['data']['api_levels']
            if str(fuchsia_api_level) in versions:
                print(
                    "error: Fuchsia API level {fuchsia_api_level} is already defined."
                    .format(fuchsia_api_level=fuchsia_api_level),
                    file=sys.stderr)
                return False

            # The API compatibility window is currently 2. This is the number
            # of stable API levels that will be supported in addition to the one in development.
            # If we change this window, the code will have to change as well.
            for level, data in versions.items():
                if data['status'] == 'in-development':
                    data['status'] = 'supported'
                elif data['status'] == 'supported':
                    data['status'] = 'unsupported'

            abi_revision = generate_random_abi_revision()
            versions[str(fuchsia_api_level)] = {
                'abi_revision': abi_revision,
                'status': 'in-development'
            }
            f.seek(0)
            json.dump(version_history, f, indent=4)
            f.truncate()
            return True
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'.
Did you run this script from the root of the source tree?""".format(
                path=version_history_path),
            file=sys.stderr)
        return False


def move_owners_file(root_source_dir, root_build_dir, fuchsia_api_level):
    """Helper function for copying golden files. It accomplishes the following:
    1. Overrides //sdk/history/OWNERS in //sdk/history/N/ allowing a wider set of reviewers.
    2. Reverts //sdk/history/N-1/  back to using //sdk/history/OWNERS, now that N-1 is a
       supported API level.

    """
    root = join_path(root_source_dir, "sdk", "history")
    src = join_path(root, str(fuchsia_api_level - 1), "OWNERS")
    dst = join_path(root, str(fuchsia_api_level))

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


def copy_compatibility_test_goldens(root_build_dir, fuchsia_api_level):
    """Updates the golden files used for compatibility testing".

    This assumes a clean build with:
      fx set core.x64 --with //sdk:compatibility_testing_goldens

    Any files that can't be copied are logged and must be updated manually.
    """
    goldens_manifest = os.path.join(
        root_build_dir, "compatibility_testing_goldens.json")

    with open(goldens_manifest) as f:
        for entry in json.load(f):
            src = join_path(root_build_dir, entry["src"])
            dst = join_path(root_build_dir, entry["dst"])
            try:
                print(f"copying {src} to {dst}")
                shutil.copyfile(src, dst)
            except Exception as e:
                print(f"failed to copy {src} to {dst}: {e}")
                return False
    return True


def join_path(root_dir, *paths):
    """Returns absolute path """
    return os.path.abspath(os.path.join(root_dir, *paths))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fuchsia-api-level", type=int, required=True)
    parser.add_argument("--sdk-version-history", required=True)
    parser.add_argument("--platform-version-json", required=True)
    parser.add_argument("--goldens-manifest", required=True)
    parser.add_argument("--fidl-compatibility-doc-path", required=True)
    parser.add_argument("--update-goldens", type=bool, default=True)
    parser.add_argument("--root-build-dir", default="out/default")
    parser.add_argument("--root-source-dir")
    parser.add_argument("--stamp-file")
    parser.add_argument("--revert-on-error", action="store_true", default=False)

    args = parser.parse_args()

    if not update_version_history(args.fuchsia_api_level,
                                  args.sdk_version_history):
        return 1

    if not update_platform_version(args.fuchsia_api_level,
                                   args.platform_version_json):
        return 1

    if not update_fidl_compatibility_doc(args.fuchsia_api_level,
                                         args.fidl_compatibility_doc_path):
        return 1

    if args.update_goldens:
        if not move_owners_file(args.root_source_dir, args.root_build_dir,
                                args.fuchsia_api_level):
            return 1
        if not copy_compatibility_test_goldens(args.root_build_dir,
                                               args.fuchsia_api_level):
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
