#!/usr/bin/env python3
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

def freeze_in_development_api_level:
    """Updates platform_version.json to add the in_development_api_level to supported_fuchsia_api_levels.
    """
    try:
        with open(PLATFORM_VERSION_PATH, "r+") as f:
            platform_version = json.load(f)
            in_development_api_level = platform_version["in_development_api_level"]
            if (in_development_api_level in platform_version["supported_fuchsia_api_levels"]):
                break
            platform_version["supported_fuchsia_api_levels"].append(in_development_api_level)
            f.seek(0)
            json.dump(platform_version, f)
            f.truncate()
    except FileNotFoundError as e:
        raise Exception("Did you run this from the source tree root?") from e


def main():
  parser = argparse.ArgumentParser(description=__doc__)
  parser.add_argument("--stamp-file")

  args = parser.parse_args()

  freeze_platform_version()


if __name__ == "__main__":
  main()