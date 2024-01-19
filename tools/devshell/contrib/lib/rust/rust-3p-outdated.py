#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Checks how out of date our third-party rust deps are

import os
from pathlib import Path
import subprocess
import json
from urllib import request, error
from datetime import datetime
from typing import List, Tuple

from rust import ROOT_PATH, PREBUILT_THIRD_PARTY_DIR

manifest = ROOT_PATH / "third_party/rust_crates/Cargo.toml"
cargo_binary = PREBUILT_THIRD_PARTY_DIR / "cargo"

cache_dir = (
    Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    / "fuchsia-rust-3p-outdated"
)
cache_dir.mkdir(exist_ok=True)

url = "https://crates.io/api/v1/crates/{}"
headers = {}
opener = request.build_opener()
opener.addheaders = [("user-agent", "fuchsia-rust-3p-outdated")]
request.install_opener(opener)


def fetch_crate_versions(crate: str) -> List[Tuple[str, datetime]]:
    cache_path = cache_dir / crate
    if not cache_path.exists():
        try:
            print("Info: fetching", crate, "release data from crates.io")
            request.urlretrieve(url.format(crate), cache_dir / crate)
        except error.HTTPError:
            print("Warning:", crate, "was not found on crates.io")
            return []
    with open(cache_path) as f:
        data = json.load(f)
    return [
        (v["num"], datetime.fromisoformat(v["created_at"]))
        for v in data["versions"]
    ]


if __name__ == "__main__":
    cargo_process = subprocess.run(
        ["cargo", "metadata", "--manifest-path", manifest],
        text=True,
        capture_output=True,
    )
    metadata = json.loads(cargo_process.stdout)
    crates = {p["name"]: p["version"] for p in metadata["packages"]}
    updates = []

    for crate, version in list(crates.items()):
        versions = fetch_crate_versions(crate)
        try:
            versions = versions[
                : next((i for i, (v, _) in enumerate(versions) if v == version))
                + 1
            ]
        except StopIteration:
            continue
        current = versions[-1][1]
        newest = versions[0][1]
        updates.append(
            (newest - current, len(versions), crate, version, versions[0])
        )

    updates.sort()
    for age, releases, crate, version, newest in updates:
        print(crate, version, "is", end=" ")
        if version != newest[0]:
            print(age.days, "days out of date")
            print(f"\t{releases} newer versions")
            print(
                f"\tmost recent release was {newest[0]} on {newest[1].date()}"
            )
        else:
            print("up to date")

    print()
    print("median outdatedness:", updates[len(updates) // 2][0].days, "days")
    print(
        f"average outdatedness: {sum(u[0].days for u in updates) // len(updates)} days"
    )
    print("most out of date:", updates[-1][0].days, "days")
    releases_behind = sorted(u[1] for u in updates)
    print()
    print("median releases behind:", releases_behind[len(releases_behind) // 2])
    print(
        f"average releases behind {sum(releases_behind) / len(releases_behind):.1f}"
    )
    print("most releases behind:", releases_behind[-1])
