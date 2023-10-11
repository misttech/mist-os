#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances.wlan import wlan


class Wlan(wlan.Wlan):
    """Wlan affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def get_phy_ids(self) -> list[int]:
        raise NotImplementedError
