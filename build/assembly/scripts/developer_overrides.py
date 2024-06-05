#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
from dataclasses import dataclass
import json
import sys
from typing import Any, Dict, List
import logging
from serialization import (
    instance_from_dict,
)


@dataclass
class ShellCommandEntryFromGN:
    package: str
    components: List[str]


def main():
    parser = argparse.ArgumentParser(
        description="Tool for creating the file for Assembly developer overrides in-tree"
    )
    parser.add_argument(
        "--input",
        required=True,
        type=argparse.FileType("r"),
        help="Path to a json file of shell commands",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Path to the output file that will contain the developer overrides",
    )
    args = parser.parse_args()

    overrides_json: Dict[str, Any] = json.load(args.input)

    # Translate the shell commands from a list to a dict
    if "shell_commands" in overrides_json:
        shell_commands = {}
        for json_entry in overrides_json["shell_commands"]:
            entry = instance_from_dict(ShellCommandEntryFromGN, json_entry)
            shell_commands[entry.package] = [
                f"bin/{name}" for name in entry.components
            ]
        overrides_json["shell_commands"] = shell_commands

    with open(args.output, "w") as output:
        json.dump(overrides_json, output, indent=2)


if __name__ == "__main__":
    sys.exit(main())
