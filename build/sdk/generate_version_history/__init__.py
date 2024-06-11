# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Preprocesses version_history.json before including it in the IDK."""

from typing import Any


def replace_special_abi_revisions_using_latest_numbered(
    version_history: Any,
) -> None:
    """Modifies version_history to assign ABI revisions for the special API
    levels.

    This is the legacy implementation, which reuses the ABI revision from the
    highest-numbered API level.

    TODO(https://fxbug.dev/324892812): Delete this method.
    """
    data = version_history["data"]

    latest_api_level = max(data["api_levels"], key=lambda l: int(l))
    latest_abi_revision = data["api_levels"][latest_api_level]["abi_revision"]

    for level, value in version_history["data"]["special_api_levels"].items():
        input_abi_revision = value["abi_revision"]
        assert (
            input_abi_revision == "GENERATED_BY_BUILD"
        ), f"ABI revision for special API level {level} was '{input_abi_revision}'; expected 'GENERATED_BY_BUILD'"
        value["abi_revision"] = latest_abi_revision


def replace_special_abi_revisions_using_commit_hash(
    version_history: Any, latest_commit_hash: str
) -> None:
    """Modifies `version_history`, assigning ABI revisions for the special API
    levels. The first 16 bits of the ABI revisions indicate whether the API
    level is HEAD or PLATFORM. The remainder is a prefix of the given git
    hash."""
    # Embed the first 48 bits of the git hash in the ABI. That should be
    # plenty to look up the full commit hash in git.
    masked_git_hash = 0x0000_FFFF_FFFF_FFFF & int(latest_commit_hash[:12], 16)

    for level, value in version_history["data"]["special_api_levels"].items():
        input_abi_revision = value["abi_revision"]
        assert (
            input_abi_revision == "GENERATED_BY_BUILD"
        ), f"ABI revision for special API level {level} was '{input_abi_revision}'; expected 'GENERATED_BY_BUILD'"
        if level == "HEAD":
            # ABI revisions for HEAD start with 0xFF00.
            value["abi_revision"] = "0x{:X}".format(
                0xFF00_0000_0000_0000 | masked_git_hash
            )
        elif level == "PLATFORM":
            # ABI revisions for PLATFORM start with 0xFF01.
            value["abi_revision"] = "0x{:X}".format(
                0xFF01_0000_0000_0000 | masked_git_hash
            )
        else:
            assert False, "Unknown special API level: %s" % level
