# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os.path as path
import sys
from typing import TextIO

import depfile

# This script outputs a summary file of SDK FIDL methods (at version HEAD).
#
# Inputs:
#  --sdk-fidl-json   : path to sdk_fidl_json.json which lists all SDK FIDL libraries
#
# Outputs:
#  --method-summary  : path to output a JSON file which summarizes library/method in the SDK
#  --depfile         : path to output a list of files this script reads


class Method:
    def __init__(self, name: str, ordinal: int) -> None:
        self.name = name
        self.ordinal = ordinal


class Api:
    def __init__(self, name: str, api_file: TextIO) -> None:
        self.methods = []
        self.name = name
        raw_json_text = api_file.read()
        if not raw_json_text:
            # Some files are empty, and json won't parse that.
            api_data = json.loads("[]")
        else:
            api_data = json.loads(raw_json_text)
        for entry in api_data:
            if entry["kind"] == "protocol/member":
                self.methods.append(
                    Method(name=entry["name"], ordinal=entry["ordinal"])
                )


def ParseMainArgs(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--sdk-fidl-json",
        help="Path to //out/*/sdk_fidl_json.json",
        required=True,
        type=argparse.FileType("r"),
    )
    parser.add_argument(
        "--method-summary",
        help="Path to write the method summary",
        required=True,
        type=argparse.FileType("w"),
    )
    parser.add_argument(
        "--depfile",
        help="Path to write the depfile listing files this script has read",
        required=True,
        type=argparse.FileType("w"),
    )
    return parser.parse_args(argv)


def Main(argv: list[str]) -> int:
    args = ParseMainArgs(argv)
    dep_paths = [args.sdk_fidl_json.name]
    with args.sdk_fidl_json as f:
        fidl_files = json.load(f)

    apis = []
    for fidl_entry in fidl_files:
        ir_file_path = fidl_entry["ir"]
        library_directory = path.dirname(ir_file_path)
        library_name = fidl_entry["name"]
        api_file_path = path.join(
            library_directory, f"{library_name}.api_summary.json"
        )
        with open(api_file_path) as api:
            apis.append(Api(name=library_name, api_file=api))
        dep_paths.append(api_file_path)

    with args.method_summary as summary_file:
        json.dump(apis, summary_file, default=vars)
    with args.depfile as d:
        depfile.DepFile.from_deps(
            "ctf_fidl_api_method_summary", dep_paths
        ).write_to(d)
    return 0


def main() -> None:
    sys.exit(Main(sys.argv[1:]))


if __name__ == "__main__":
    sys.exit(Main(sys.argv[1:]))
