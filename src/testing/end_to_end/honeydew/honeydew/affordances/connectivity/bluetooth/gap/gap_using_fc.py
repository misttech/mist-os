# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Gap affordance implementation using Fuchsia Controller."""

from honeydew.affordances.connectivity.bluetooth.bluetooth_common import (
    bluetooth_common_using_fc,
)
from honeydew.affordances.connectivity.bluetooth.gap import gap


class GapUsingFc(bluetooth_common_using_fc.BluetoothCommonUsingFc, gap.Gap):
    """BluetoothGap Common affordance implementation using Fuchsia Controller.

    Args:
        device_name: Device name returned by `ffx target list`.
        fuchsia_controller: Fuchsia Controller transport.
    """
