# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Preprocesses version_history.json before including it in the IDK."""

from typing import Any


def replace_special_abi_revisions(version_history: Any) -> None:
    """Modifies version_history to assign ABI revisions for the special API
    levels."""
    data = version_history["data"]

    # TODO(https://fxbug.dev/324892812): Use a per-release ABI revision, not the
    # latest stable one.
    latest_api_level = max(data["api_levels"], key=lambda l: int(l))
    latest_abi_revision = data["api_levels"][latest_api_level]["abi_revision"]

    for level, value in version_history["data"]["special_api_levels"].items():
        input_abi_revision = value["abi_revision"]
        assert (
            input_abi_revision == "GENERATED_BY_BUILD"
        ), f"ABI revision for special API level {level} was '{input_abi_revision}'; expected 'GENERATED_BY_BUILD'"
        value["abi_revision"] = latest_abi_revision
