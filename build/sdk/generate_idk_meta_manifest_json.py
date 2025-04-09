#!/usr/bin/env fuchsia-vendored-python
# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generate an IDK meta/manifest.json file for all atoms described by an internal SDK manifest file."""

import argparse
import json
import sys
import typing as T

from sdk_common import Atom

# The type of a list of IDK parts, as it appears in the "parts"
# field of the output manifest.
IdkPart: T.TypeAlias = dict[str, str]


def get_sorted_parts(atoms: list[Atom]) -> T.Sequence[IdkPart]:
    def key(ad: IdkPart) -> tuple[str, str]:
        return (ad["meta"], ad["type"])

    return sorted(
        (
            {
                "meta": a.metadata,
                "stable": a.stable,
                "type": a.type,
            }
            for a in atoms
        ),
        key=key,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        help="Path to input SDK manifest describing all IDK atoms.",
        required=True,
    )
    parser.add_argument("--meta", help="Path to output file.", required=True)
    parser.add_argument(
        "--target-arch",
        help="Architecture of precompiled target atoms",
        required=True,
    )
    parser.add_argument(
        "--host-arch", help="Architecture of host tools", required=True
    )
    parser.add_argument(
        "--id", help="Opaque identifier for the SDK", default=""
    )
    parser.add_argument(
        "--schema-version",
        help="Opaque identifier for the metadata schemas",
        required=True,
    )
    args = parser.parse_args()

    with open(args.manifest, "r") as manifest_file:
        manifest = json.load(manifest_file)

    # sdk_noop_atoms may contain empty meta, skip them.
    atoms = [Atom(a) for a in manifest["atoms"] if a["meta"]]
    meta = {
        "arch": {
            "host": args.host_arch,
            "target": [
                args.target_arch,
            ],
        },
        "id": args.id,
        "parts": get_sorted_parts(atoms),
        "root": manifest["root"],
        "schema_version": args.schema_version,
    }

    with open(args.meta, "w") as meta_file:
        json.dump(
            meta, meta_file, indent=2, sort_keys=True, separators=(",", ": ")
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
