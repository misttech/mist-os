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


def main():
    parser = argparse.ArgumentParser(
        'Processes version_history.json to return list of supported and in-development API levels.'
    )
    parser.add_argument(
        '--version-history-path',
        type=Path,
        help='Path to the version history JSON file')

    args = parser.parse_args()

    try:
        versions = get_supported_versions(args.version_history_path)
    except FileNotFoundError as e:
        print(f"Error: {e}")
        return 1

    if not versions:
        return 1

    print(json.dumps(versions))
    return 0


def get_supported_versions(version_history_path: Path) -> Dict[str, Any]:
    """Reads from version_history.json to get supported and in-development API levels.
    """

    try:
        with open(version_history_path) as file:
            data = json.load(file)
    except FileNotFoundError:
        print(
            """error: Unable to open '{path}'. Did you run this script from the root of the source tree?"""
            .format(path=version_history_path),
            file=sys.stderr)
        return None

    api_levels = data['data']['api_levels']
    in_development_api_level = None
    supported_fuchsia_api_levels = []

    for api_level, info in api_levels.items():
        status = info['status']
        if status == 'in-development':
            in_development_api_level = int(api_level)
        elif status == 'supported':
            supported_fuchsia_api_levels.append(int(api_level))

    result = {
        "in_development_api_level": in_development_api_level,
        "supported_fuchsia_api_levels": supported_fuchsia_api_levels
    }

    return result


if __name__ == '__main__':
    sys.exit(main())
