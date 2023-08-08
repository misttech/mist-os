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

PLATFORM_VERSION_PATH = "build/config/fuchsia/platform_version.json"
VERSION_HISTORY_PATH = "sdk/version_history.json"


def freeze_in_development_api_level():
    try:
        # Update version_history.json to freeze the api level
        with open(VERSION_HISTORY_PATH, "r+") as f:
            version_history = json.load(f)
        new_version_history = freeze_version_history(version_history)
        with open(VERSION_HISTORY_PATH, "w") as f:
            json.dump(new_version_history, f)

        # Update platform_version.json based on version_history.json
        with open(VERSION_HISTORY_PATH, "r+") as f:
            version_history = json.load(f)
        new_platform_version = version_history_to_platform_version(
            version_history)
        with open(PLATFORM_VERSION_PATH, "w") as f:
            json.dump(new_platform_version, f)

    except FileNotFoundError as e:
        raise Exception("Did you run this from the source tree root?") from e


def freeze_version_history(version_history):
    """Updates version_history.json to make the in_development_api_level supported."""
    for level, info in version_history["data"]["api_levels"].items():
        if info["status"] == "in-development":
            info["status"] = "supported"
            break
    return version_history


def version_history_to_platform_version(version_history):
    """Given a JSON object for `version_history.json`, generate a corresponding one for
    `platform_version.json`"""
    versions = version_history['data']['api_levels']

    in_development_api_levels = [
        int(level)
        for level, data in versions.items()
        if data['status'] == 'in-development'
    ]
    supported_levels = [
        int(level)
        for level, data in versions.items()
        if data['status'] == 'supported'
    ]

    if len(in_development_api_levels) == 0:
        # This is a kind of weird state: when there's no in-development API
        # level, this is the API freeze. However, `platform_version.json`
        # assumes that there's always an `in_development_api_level`, so we pick
        # the largest supported level to fill that role.
        # We'll remove `platform_version.json` soon enough, so whatever.
        in_development_api_level = max(supported_levels)
    elif len(in_development_api_levels) == 1:
        in_development_api_level = in_development_api_levels[0]
    else:
        raise Exception(
            """error: expected no more than 1 in-development API level in version_history.json;
            found {}""".format(len(in_development_api_levels)))

    return dict(
        in_development_api_level=in_development_api_level,
        supported_fuchsia_api_levels=supported_levels)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    # This arg is necessary for the builder to work, even though it isn't used.
    parser.add_argument("--stamp-file")
    args = parser.parse_args()

    freeze_in_development_api_level()


if __name__ == "__main__":
    sys.exit(main())
