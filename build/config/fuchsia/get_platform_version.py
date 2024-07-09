#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Gets relevant supported and in development Fuchsia platform versions from main config file.
"""

import argparse
import json
from pathlib import Path
import sys
from typing import Any, Dict


def main() -> int:
    parser = argparse.ArgumentParser(
        "Processes version_history.json to return list of supported and in-development API levels."
    )
    parser.add_argument(
        "--version-history-path",
        type=Path,
        required=True,
        help="Path to the version history JSON file",
    )

    args = parser.parse_args()
    print(json.dumps(get_gn_variables(args.version_history_path)))
    return 0


def get_gn_variables(version_history_path: Path) -> Dict[str, Any]:
    """Reads from version_history.json to generate some data to expose to the GN
    build graph."""

    try:
        with open(version_history_path) as file:
            data = json.load(file)
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'. Did you run this script from the root of the source tree?""".format(
                path=version_history_path
            ),
            file=sys.stderr,
        )
        raise

    api_levels = data["data"]["api_levels"]

    all_numbered_api_levels = [int(level) for level in api_levels]

    sunset_api_levels: list[int] = [
        int(level)
        for level in api_levels
        if api_levels[level]["status"] == "sunset"
    ]
    supported_api_levels: list[int] = [
        int(level)
        for level in api_levels
        if api_levels[level]["status"] == "supported"
    ]

    # TODO(https://fxbug.dev/326277078): Remove this and rename
    # `in_development_special_api_levels` when adding "NEXT".`
    in_development_api_levels: list[int] = [
        int(level)
        for level in api_levels
        if api_levels[level]["status"] == "in-development"
    ]

    assert (
        len(in_development_api_levels) < 2
    ), f"Should be at most one in-development API level. Found: {in_development_api_levels}"

    # Exclude "PLATFORM" because it represents a set of other API levels.
    special_api_levels = data["data"]["special_api_levels"]
    in_development_special_api_levels: list[str] = [
        str(level)
        for level in special_api_levels
        if level != "PLATFORM"
        and special_api_levels[level]["status"] == "in-development"
    ]

    idk_buildable_api_levels = supported_api_levels + in_development_api_levels
    runtime_supported_api_levels = sunset_api_levels + idk_buildable_api_levels

    # Temporarily do not support the numbered in-development API level, which
    # will soon be replaced with NEXT, in the IDK since so IDK consumers do not
    # start using it.
    # TODO(https://fxbug.dev/326277078): Remove this when transitioning to NEXT.
    for level in in_development_api_levels:
        idk_buildable_api_levels.remove(level)

    # The special API levels cannot currently be targeted in the IDK and thus
    # must be added here.
    runtime_supported_api_levels += in_development_special_api_levels

    return {
        # All numbered API levels in the JSON file.
        # TODO(https://fxbug.dev/326277078): Add that these levels are all
        # frozen once "NEXT" is implemented.
        "all_numbered_api_levels": all_numbered_api_levels,
        # API levels that the IDK supports targeting.
        "idk_buildable_api_levels": idk_buildable_api_levels,
        # API levels that the platform build supports at runtime.
        "runtime_supported_api_levels": runtime_supported_api_levels,
        # API levels whose contents should not change anymore.
        "frozen_api_levels": sunset_api_levels + supported_api_levels,
        # The greatest number assigned to a numbered API level.
        # TODO(https://fxbug.dev/305961460): Remove this because the highest
        # numbered API level should not be particularly special.
        "deprecated_highest_numbered_api_level": max(all_numbered_api_levels),
    }


if __name__ == "__main__":
    sys.exit(main())
