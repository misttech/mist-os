#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan affordance implementation using SL4F."""

import enum
import logging

from honeydew.interfaces.affordances.wlan import wlan
from honeydew.transports.sl4f import SL4F

_LOGGER: logging.Logger = logging.getLogger(__name__)


class _Sl4fMethods(enum.StrEnum):
    """Sl4f server commands."""

    GET_PHY_IDS = "wlan.get_phy_id_list"


class Wlan(wlan.Wlan):
    """Wlan affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """

    def __init__(self, device_name: str, sl4f: SL4F) -> None:
        self._name: str = device_name
        self._sl4f: SL4F = sl4f

    # List all the public methods
    def get_phy_ids(self) -> list[int]:
        """Get list of phy ids located on device.

        Returns:
            A list of phy ids that is present on the device.
        Raises:
            errors.Sl4fError: On failure.
        """
        phy_ids = self._sl4f.run(method=_Sl4fMethods.GET_PHY_IDS)
        return phy_ids.get("result", [])
