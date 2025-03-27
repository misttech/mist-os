#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import sys

import json5


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate an offer shard for Starnix container config in a test."
    )
    parser.add_argument("--schema", type=argparse.FileType("r"), required=True)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    parser.add_argument("--offer-from", type=str, required=True)
    parser.add_argument("--offer-to", type=str, required=True)
    args = parser.parse_args()

    with args.schema as f:
        schema = json5.load(f)

    offer = []
    for cap_schema in schema["use"]:
        offer.append(
            {
                "config": cap_schema["config"],
                "from": args.offer_from,
                "to": args.offer_to,
            }
        )

    shard = {
        "offer": offer,
    }

    with args.output as f:
        f.write(json.dumps(shard))

    return 0


if __name__ == "__main__":
    sys.exit(main())
