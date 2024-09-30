# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from argparse import ArgumentParser, Namespace
from pathlib import Path
from sys import argv

# used by //build/rust/tests to test that rustdoc-link generates complete docs.


def main(args: Namespace):
    debug_tree = list(args.directory.glob("**/*"))
    assert (
        args.directory.is_dir()
    ), f"expected `{args.directory}` to be a directory. tree: {repr(debug_tree)}"
    found = [
        str(p.relative_to(args.directory)) for p in args.directory.iterdir()
    ]
    for c in args.contains:
        assert (
            c in found
        ), f"expected `{repr(found)}` to contain `{c}`. tree: {repr(debug_tree)}"
    for c in args.absent:
        assert (
            c not in found
        ), f"expected `{repr(found)}` not to contain `{c}`. tree: {repr(debug_tree)}"
    if args.touch is not None:
        args.touch.touch()


def _main_arg_parser() -> ArgumentParser:
    parser = ArgumentParser(
        description="checks (exists, contains) about a file. exits unsuccessfully if properties could not be verified"
    )
    parser.add_argument(
        "--directory",
        required=True,
        type=Path,
        help="directory to base the file search on",
    )
    parser.add_argument(
        "--contains",
        action="append",
        default=[],
        type=str,
        help="optional: check this file or dir for the given content",
    )
    parser.add_argument(
        "--absent",
        action="append",
        default=[],
        type=str,
        help="make sure the matched file or directory does not have this content",
    )
    parser.add_argument(
        "--touch",
        type=Path,
        help="write to this file upon test pass",
    )
    parser.set_defaults(func=main)

    return parser


if __name__ == "__main__":
    parser = _main_arg_parser()
    args = parser.parse_args(argv[1:])
    args.func(args)
