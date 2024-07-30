# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Preprocesses version_history.json before including it in the IDK."""

import datetime
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


def replace_special_abi_revisions_using_commit_hash_and_date(
    version_history: Any,
    daily_commit_hash: str,
    daily_commit_timestamp: datetime.datetime,
) -> None:
    """Modifies `version_history`, assigning ABI revisions for the special API
    levels. The format is the following:

        0xFF0X_YYYY_ZZZZ_ZZZZ

        X: 0 if the API level is NEXT or HEAD. 1 if the API level is PLATFORM.
        YYYY: the first 16 bits of `daily_commit_hash`. This is largely to make
            the ABI revision unpredictable.
        ZZZZ_ZZZZ: 32 bit encoding of the proleptic Gregorian ordinal of the day
            *after* `daily_commit_timestamp`. In most situations, this will
            indicate "today". We'll need to revise this encoding sometime before
            the year 11761200.
    """
    # Embed the first 16 bits of the git hash in the ABI.
    masked_git_hash = (0xFFFF & int(daily_commit_hash[:4], 16)) << 32

    masked_day = 0xFFFF_FFFF & (daily_commit_timestamp.toordinal() + 1)

    for level, value in version_history["data"]["special_api_levels"].items():
        input_abi_revision = value["abi_revision"]
        assert (
            input_abi_revision == "GENERATED_BY_BUILD"
        ), f"ABI revision for special API level {level} was '{input_abi_revision}'; expected 'GENERATED_BY_BUILD'"
        if level == "NEXT" or level == "HEAD":
            # ABI revisions for unstable levels start with 0xFF00.
            # All such levels share an ABI revision.
            value["abi_revision"] = "0x{:X}".format(
                0xFF00_0000_0000_0000 | masked_git_hash | masked_day
            )
        elif level == "PLATFORM":
            # ABI revisions for PLATFORM start with 0xFF01.
            value["abi_revision"] = "0x{:X}".format(
                0xFF01_0000_0000_0000 | masked_git_hash | masked_day
            )
        else:
            assert False, "Unknown special API level: %s" % level
