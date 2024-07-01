#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Freezes the Fuchsia platform version.
"""

import argparse
import json
import os
import sys
from typing import Any


def freeze_in_development_api_level(version_history_path: str) -> str:
    """Updates version_history.json to freeze the current in-development API
    level and returns that level."""
    try:
        with open(version_history_path, "r+") as f:
            version_history = json.load(f)
        (level_frozen, new_version_history) = freeze_version_history(
            version_history
        )
        with open(version_history_path, "w") as f:
            json.dump(new_version_history, f, indent=4)
        return level_frozen
    except FileNotFoundError as e:
        raise Exception("Did you run this from the source tree root?") from e


def freeze_version_history(
    version_history: dict[str, Any]
) -> tuple[str, dict[str, Any]]:
    """Updates version_history.json to make the in_development_api_level supported."""
    for level, info in version_history["data"]["api_levels"].items():
        if info["status"] == "in-development":
            info["status"] = "supported"
            return (level, version_history)
    raise Exception("No in-development API level found.")


def update_owners_file(root_source_dir: str, fuchsia_api_level: str) -> str:
    """Updates the OWNERS file with more limited access for frozen API levels.
    Returns the path to the file.
    """
    level_dir_path = os.path.join(
        root_source_dir, "sdk", "history", fuchsia_api_level
    )
    owners_path = os.path.join(level_dir_path, "OWNERS")

    print(f"Updating {owners_path}")
    with open(owners_path, "w") as f:
        f.write("include /sdk/history/FROZEN_API_LEVEL_OWNERS\n")

    return owners_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    # This arg is necessary for the builder to work, even though it isn't used.
    parser.add_argument("--stamp-file")
    parser.add_argument("--sdk-version-history", required=True)
    parser.add_argument("--root-source-dir", required=True)
    args = parser.parse_args()

    level_frozen = freeze_in_development_api_level(args.sdk_version_history)

    owners_path = update_owners_file(args.root_source_dir, level_frozen)

    # Before printing, rebase the paths to what a developer would use from `//`.
    history = os.path.relpath(args.sdk_version_history, args.root_source_dir)
    owners = os.path.relpath(owners_path, args.root_source_dir)
    print(f"Run `git add -u {history} {owners}`.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
