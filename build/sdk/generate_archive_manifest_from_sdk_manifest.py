#!/usr/bin/env fuchsia-vendored-python
# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generate a tarmaker manifest from an input SDK manifest describing a set of atoms."""

import argparse
import json
import os
import sys
from typing import Any, Sequence

try:
    # Python 3
    from urllib.parse import urlparse
except ImportError:
    from urllib.parse import urlparse as urlparse  # noqa: F811

from sdk_common import Atom


class MappingAction(argparse.Action):
    """Parses file mappings flags."""

    def __init__(
        self,
        option_strings: list[str],
        dest: str,
        nargs: int | None = None,
        **kwargs: Any,
    ) -> None:
        if nargs is not None:
            raise ValueError("nargs is not allowed")
        super(MappingAction, self).__init__(
            option_strings, dest, nargs=2, **kwargs
        )

    def __call__(
        self,
        parser: argparse.ArgumentParser,
        namespace: argparse.Namespace,
        values: Any,
        option_string: str | None = None,
    ) -> None:
        mappings = getattr(namespace, "mappings", None)
        if mappings is None:
            mappings = {}
            setattr(namespace, "mappings", mappings)
        mappings[values[0]] = values[1]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest", help="Path to the input SDK manifest file.", required=True
    )
    parser.add_argument(
        "--mapping",
        help="Extra files to add to the archive. Format is '--mapping <dest> <source>'.",
        action=MappingAction,
    )
    parser.add_argument(
        "--output", help="Path to the output tarmaker manifest.", required=True
    )
    args = parser.parse_args()

    with open(args.manifest, "r") as manifest_file:
        manifest = json.load(manifest_file)

    all_files: dict[str, str] = {}

    def add(dest_path: str, src_path: str) -> int:
        dest = os.path.normpath(dest_path)
        src = os.path.normpath(src_path)
        if dest in all_files:
            # `sdk://packages/blobs/` and `sdk://packages/subpackage_manifests/`
            # directories may contain duplicate files, named as the hash of their
            # content. File content will match, and no concern on collision.
            _ignored_prefixes = (
                "packages/blobs/",
                "packages/subpackage_manifests/",
            )
            if dest.startswith(_ignored_prefixes):
                pass
            else:
                print("Error: multiple entries for %s" % dest)
                print("  - %s" % all_files[dest])
                print("  - %s" % src)
                return 1
        all_files[dest] = src
        return 0

    for atom in [Atom(a) for a in manifest["atoms"]]:
        for file in atom.files:
            rc = add(file.destination, file.source)
            if rc != 0:
                return rc

    for dest, source in args.mappings.items():
        rc = add(dest, source)
        if rc != 0:
            return rc

    with open(args.output, "w") as output_file:
        for dest, src in sorted(all_files.items()):
            dest = os.path.relpath(dest)
            src = os.path.relpath(src)
            output_file.write("%s=%s\n" % (dest, src))

    return 0


if __name__ == "__main__":
    sys.exit(main())
