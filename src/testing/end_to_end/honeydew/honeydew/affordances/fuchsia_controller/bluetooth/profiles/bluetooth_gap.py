# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Gap affordance implementation using Fuchsia Controller."""

from honeydew.affordances.fuchsia_controller.bluetooth import bluetooth_common
from honeydew.interfaces.affordances.bluetooth.profiles import bluetooth_gap


class BluetoothGap(
    bluetooth_gap.BluetoothGap, bluetooth_common.BluetoothCommon
):
    """BluetoothGap Common affordance implementation using Fuchsia Controller.

    Args:
        device_name: Device name returned by `ffx target list`.
        fuchsia_controller: Fuchsia Controller transport.
    """
