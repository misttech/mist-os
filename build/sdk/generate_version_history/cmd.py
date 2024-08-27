# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Preprocesses version_history.json before including it in the IDK."""

import argparse
import datetime
import json
import pathlib
from typing import Any

import generate_version_history


def main() -> None:
    parser = argparse.ArgumentParser(__doc__, allow_abbrev=False)
    parser.add_argument(
        "--input",
        type=pathlib.Path,
        required=True,
        help="Unprocessed version of version_history.json",
    )

    # TODO(https://fxbug.dev/324892812): Delete this.
    parser.add_argument(
        "--legacy-unstable-abi-revisions",
        action="store_true",
        help=(
            "When passed, uses the 'legacy' way of assigning an ABI revision "
            "to unstable API levels (e.g., `HEAD`, `PLATFORM`)"
        ),
    )

    parser.add_argument(
        "--daily-commit-hash-file",
        type=pathlib.Path,
        help=(
            "File containing the hash of the latest commit to integration.git "
            "before today, as a hexadecimal UTF-8 string. Required unless "
            "--legacy-unstable-abi-revisions is specified."
        ),
    )

    parser.add_argument(
        "--daily-commit-timestamp-file",
        type=pathlib.Path,
        help=(
            "File containing the commit timestamp of the latest commit to "
            "integration.git before today, as a decimal UNIX timestamp. "
            "Required unless --legacy-unstable-abi-revisions is specified."
        ),
    )

    parser.add_argument(
        "--output",
        type=pathlib.Path,
        required=True,
        help="Generated version of version_history.json",
    )

    args = parser.parse_args()

    with args.input.open() as f:
        version_history = json.load(f)

    if args.legacy_unstable_abi_revisions:
        generate_version_history.replace_special_abi_revisions_using_latest_numbered(
            version_history
        )
    else:
        with args.daily_commit_hash_file.open() as f:
            daily_commit_hash = f.read().strip()
        with args.daily_commit_timestamp_file.open() as f:
            daily_commit_timestamp = datetime.datetime.fromtimestamp(
                int(f.read().strip()), datetime.UTC
            )
        generate_version_history.replace_special_abi_revisions_using_commit_hash_and_date(
            version_history, daily_commit_hash, daily_commit_timestamp
        )

    with args.output.open("w") as f:
        json.dump(version_history, f, indent=2)


if __name__ == "__main__":
    main()
