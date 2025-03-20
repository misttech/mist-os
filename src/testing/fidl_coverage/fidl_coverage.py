# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This tool is to be invoked by the CTF trybot.  It is intended for
# comparing the FIDL methods that were exercised in CTF tests
# to the FIDL methods that exist in the SDK.

import argparse
import json
import os
import sys
from io import TextIOWrapper
from pathlib import Path
from typing import Any, Dict, List

# TODO(b/379766709) (comment 21): Rewrite this in Go.


# Output schema for a single FIDL method call: sender and receiver URL, and the path
# to the test's output directory (from which the test name can be extracted)
class Call:
    def __init__(
        self,
        sender: Dict[str, Any],
        receiver: Dict[str, Any],
        test_path: str,
        direction: str,
    ):
        self.sender = sender
        self.receiver = receiver
        self.test_path = test_path
        self.direction = direction


UNWANTED_CALL_KEYS = ["pid", "is_test"]


def cleanup_call(call: Dict[str, Any]) -> Dict[str, Any]:
    for key in UNWANTED_CALL_KEYS:
        if key in call:
            del call[key]
    return call


# Output schema for a single FIDL message. `name` is the name of the message, as
# `fuchsia.library/Protocol.Message` - unless the name is not in the CTF ordinal<>message
# map, in which case `name` is the same as `ordinal`. `calls` is a list of Call.
class Message:
    def __init__(self, name: str, ordinal: str) -> None:
        self.name = name
        self.ordinal = ordinal
        self.calls: List[Call] = []

    def add_calls(
        self, calls: List[Dict[str, Any]], test_path: Path, direction: str
    ) -> None:
        for call in calls:
            self.calls.append(
                Call(
                    sender=cleanup_call(call["sender"]),
                    receiver=cleanup_call(call["receiver"]),
                    test_path=str(test_path.parent),
                    direction=direction,
                )
            )


# Main data structure of the program. Gathers data from all input files and outputs it
# to JSON in the correct schema - a list of Message with an item for every entry in the
# SDK API name<>ordinal map file (whether or not it has calls) and for every event in
# every scanned FIDL-snoop file (whether or not it's in the API).
class Methods:
    def __init__(self, message_file: TextIOWrapper) -> None:
        self.setup_data_structures(message_file)

    def setup_data_structures(self, message_file: TextIOWrapper) -> None:
        self.raw_methods = json.load(message_file)
        self.name_lookup: Dict[str, Message] = {}
        self.ordinal_lookup: Dict[str, Message] = {}
        # Schema is [{name: "fuchsia.library",
        #             methods: [{name: "fuchsia.library/Protocol.Message", ordinal: 1234}, ...]}, ...]
        for entry in self.raw_methods:
            for method in entry["methods"]:
                message = Message(
                    name=method["name"], ordinal=method["ordinal"]
                )
                self.name_lookup[method["name"]] = message
                self.ordinal_lookup[method["ordinal"]] = message

    def process_calls(self, path: Path, direction: str) -> None:
        with open(path) as f:
            calls = json.load(f)
            # Schema is { message: [call, call...], message: [call, call...]...}
            #   message is either an ordinal (as a string) or "fuchsia.library/Protocol/Message"
            #   call is a dict: {"sender": process, "receiver": process}
            #     process is a dict with keys: name, is_test, pid, url, moniker
            for message, call_list in calls.items():
                if message in self.ordinal_lookup:
                    self.ordinal_lookup[message].add_calls(
                        calls=call_list, test_path=path, direction=direction
                    )
                else:
                    if message not in self.name_lookup:
                        self.name_lookup[message] = Message(
                            name=message, ordinal=message
                        )
                    self.name_lookup[message].add_calls(
                        calls=call_list, test_path=path, direction=direction
                    )

    def write_to_json(self, path: Path) -> None:
        """Writes the desired data to stdout in JSON format."""

        def vars_or_obj(obj: Any) -> Any:
            """Extracts fields from classes, and passes through built-in data types."""
            try:
                return vars(obj)
            except:
                return obj

        with open(path, "w") as f:
            json.dump(
                list(self.name_lookup.values()),
                f,
                indent=2,
                default=vars_or_obj,
            )


# The core of this script. Scans for intra_calls files, accumulates calls from them, and writes
# the accumulated information to stdout in the correct schema.
def check_coverage(args: argparse.Namespace) -> None:
    api_path = Path(args.api_file)
    with open(api_path) as f:
        methods = Methods(f)
    for root_dir in args.results_dir:
        for dir_path, _, file_names in os.walk(root_dir):
            for name in file_names:
                if name.endswith("intra_calls.freeform.json"):
                    methods.process_calls(Path(dir_path) / name, "intra")
                elif name.endswith("incoming_calls.freeform.json"):
                    methods.process_calls(Path(dir_path) / name, "incoming")
                elif name.endswith("outgoing_calls.freeform.json"):
                    methods.process_calls(Path(dir_path) / name, "outgoing")
    methods.write_to_json(Path(args.json_output))


def main(argv: List[str]) -> None:
    """This script takes an API file summarizing SDK FIDL methods (name <-> ordinal) and one or
    more directories containing test outputs, which should include FIDL-snoop files named
    `intra_calls.freeform.json`."""
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(required=True)

    subparser = subparsers.add_parser(
        "check_coverage",
        help="Summarize FIDL coverage from an API file and test result directories.",
    )
    subparser.add_argument(
        "--api_file",
        required=True,
        help="File summarizing FIDL API, produced by summarize_fidl_methods.py.",
    )
    subparser.add_argument(
        "--json_output",
        required=True,
        help="Path to write FIDL coverage summary to.",
    )
    subparser.add_argument(
        "--results_dir", nargs="+", help="Root directories of test outputs"
    )
    subparser.set_defaults(func=check_coverage)

    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main(sys.argv[1:])
