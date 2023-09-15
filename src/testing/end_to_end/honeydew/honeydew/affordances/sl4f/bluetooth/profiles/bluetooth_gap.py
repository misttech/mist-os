#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Gap affordance implementation using SL4F."""

from honeydew.affordances.sl4f.bluetooth import bluetooth_common
from honeydew.interfaces.affordances.bluetooth.profiles import bluetooth_gap


class BluetoothGap(bluetooth_common.BluetoothCommon,
                   bluetooth_gap.BluetoothGap):
    """BluetoothGap Common affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """
