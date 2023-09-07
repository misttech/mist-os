#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Compare the contents of an IDK to a golden file.

This script reads the manifest.json file from an exported IDK directory and
compares its contents to one or more existing golden files. If the contents of
the IDK don't match the contents of the golden file, this returns an error
indicating the set of changes that should be acknowledged by updating the golden
file.

Used by `sdk_final_manifest_golden` GN rules.
"""

import argparse
import json
import os
import sys
from typing import Sequence, TypedDict

HOST_TOOL_SCHEMES: Sequence[str] = [
    "host_tool", "ffx_tool", "companion_host_tool"
]

IdkPart = TypedDict("IdkPart", {"meta": str, "type": str})
IdkManifest = TypedDict("IdkManifest", {"parts": Sequence[IdkPart]})


def part_to_id(part: IdkPart) -> str:
    """Get a string ID for a part of the IDK.

    Args:
    - part: An entry from the `parts` field of the IDK manifest.

    Returns:
      A string ID for that part, that's a bit more human-friendly than the
      JSON object.
    """
    path = part["meta"]
    if os.path.basename(path) == "meta.json":
        path = os.path.dirname(path)
    elif path.endswith("-meta.json"):
        path = path[:-len("-meta.json")]

    return f'{part["type"]}://{path}'


def part_is_tool_for_other_cpu(part_id: str, host_cpu: str) -> bool:
    """Returns True if a given part is a host tool for a different cpu."""
    for scheme in HOST_TOOL_SCHEMES:
        if part_id.startswith(f"{scheme}://") and not part_id.startswith(
                f"{scheme}://tools/{host_cpu}/"):
            return True
    return False


def remove_tools_for_other_cpus(part_ids: set[str], cpu: str) -> set[str]:
    """Takes a set of IDs and returns a set excluding host tools built
    for other CPU architectures."""
    return {id for id in part_ids if not part_is_tool_for_other_cpu(id, cpu)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest", help="Path to the IDK manifest", required=True)
    parser.add_argument(
        "--golden", help="Path to the golden file", required=True)
    parser.add_argument(
        "--inherit_golden",
        help=(
            "Inherit items from this golden file, if specified, and omit "
            "any items from this file in the updated golden."),
        required=False,
    )
    parser.add_argument(
        "--only_verify_host_tools_for_cpu",
        help=
        "If specified, ignore any host tools that aren't built for the named CPU arch.",
        required=False,
    )
    parser.add_argument(
        "--updated_golden",
        help=(
            "Path where a new version of the golden file will be written iff "
            "--only_verify_host_tools_for_cpu is not specified"),
        required=False,
    )

    args = parser.parse_args()

    with open(args.manifest) as f:
        manifest: IdkManifest = json.load(f)
    manifest_ids = {part_to_id(part) for part in manifest["parts"]}

    with open(args.golden) as f:
        golden_ids = {line.strip() for line in f}

    if args.inherit_golden:
        with open(args.inherit_golden) as f:
            inherit_golden_ids = {line.strip() for line in f}
    else:
        inherit_golden_ids = set()

    effective_golden_ids = golden_ids | inherit_golden_ids

    added_ids: set[str] = manifest_ids - effective_golden_ids
    removed_ids: set[str] = effective_golden_ids - manifest_ids

    if args.only_verify_host_tools_for_cpu:
        added_ids = remove_tools_for_other_cpus(
            added_ids, args.only_verify_host_tools_for_cpu)
        removed_ids = remove_tools_for_other_cpus(
            removed_ids, args.only_verify_host_tools_for_cpu)
    else:
        assert args.updated_golden, (
            "--updated_golden must be specified unless --only_verify_host_tools_for_cpu "
            "is specified")

        with open(args.updated_golden, "w") as f:
            for id in sorted(manifest_ids - inherit_golden_ids):
                print(id, file=f)

    if added_ids:
        print("Parts added to IDK:", file=sys.stderr)
        for id in sorted(added_ids):
            print(" - " + id, file=sys.stderr)
    if removed_ids:
        print("Parts removed from IDK:", file=sys.stderr)
        for id in sorted(removed_ids):
            print(" - " + id, file=sys.stderr)
    if removed_ids or added_ids:
        print("Error: IDK contents have changed!", file=sys.stderr)
        if args.only_verify_host_tools_for_cpu:
            print(
                f"""\
The manifest cannot be automatically updated when not cross compiling host tools.

Please update the manifest manually - {args.golden}

or run fx set with "--args sdk_cross_compile_host_tools=true"
""",
                file=sys.stderr,
            )
        else:
            print(
                f"""\
Please acknowledge this change by running:

  cp {os.path.abspath(args.updated_golden)} {os.path.abspath(args.golden)}
""",
                file=sys.stderr,
            )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
