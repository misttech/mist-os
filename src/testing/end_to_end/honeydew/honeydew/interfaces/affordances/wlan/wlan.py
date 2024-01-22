#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for wlan affordance."""

import abc


class Wlan(abc.ABC):
    """Abstract base class for Wlan affordance."""

    # List all the public methods
    @abc.abstractmethod
    def get_phy_ids(self) -> list[int]:
        """Get a list of identifiers for WLAN PHYs.

        Returns:
            A list of PHY IDs.
        """
