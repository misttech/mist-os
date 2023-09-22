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

VERSION_HISTORY_PATH = "sdk/version_history.json"


def freeze_in_development_api_level():
    try:
        # Update version_history.json to freeze the api level
        with open(VERSION_HISTORY_PATH, "r+") as f:
            version_history = json.load(f)
        new_version_history = freeze_version_history(version_history)
        with open(VERSION_HISTORY_PATH, "w") as f:
            json.dump(new_version_history, f)

    except FileNotFoundError as e:
        raise Exception("Did you run this from the source tree root?") from e


def freeze_version_history(version_history):
    """Updates version_history.json to make the in_development_api_level supported."""
    for level, info in version_history["data"]["api_levels"].items():
        if info["status"] == "in-development":
            info["status"] = "supported"
            break
    return version_history


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    # This arg is necessary for the builder to work, even though it isn't used.
    parser.add_argument("--stamp-file")
    args = parser.parse_args()

    freeze_in_development_api_level()


if __name__ == "__main__":
    sys.exit(main())
