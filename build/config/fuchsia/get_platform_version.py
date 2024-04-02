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

    supported_api_levels: list[int] = [
        int(level)
        for level in api_levels
        if api_levels[level]["status"] == "supported"
    ]
    in_development_api_levels: list[int] = [
        int(level)
        for level in api_levels
        if api_levels[level]["status"] == "in-development"
    ]

    assert (
        len(in_development_api_levels) < 2
    ), f"Should be at most one in-development API level. Found: {in_development_api_levels}"

    # Sometimes, there actually *isn't* an "in development" API level, and GN
    # doesn't support null values. As a hack, if there's no in-development API
    # level, just say that the greatest supported level is "in-development",
    # even though it isn't.
    #
    # TODO: https://fxbug.dev/305961460 - Remove this special case.
    if not in_development_api_levels:
        in_development_api_level: int = max(supported_api_levels)
    else:
        in_development_api_level = in_development_api_levels[0]

    return {
        "all_numbered_api_levels": all_numbered_api_levels,
        # TODO: https://fxbug.dev/305961460 - Remove this.
        "in_development_api_level": in_development_api_level,
        # API levels that the IDK supports targeting.
        "build_time_supported_api_levels": (
            supported_api_levels + in_development_api_levels
        ),
        # API levels whose contents should not change anymore.
        "frozen_api_levels": supported_api_levels,
    }


if __name__ == "__main__":
    sys.exit(main())
