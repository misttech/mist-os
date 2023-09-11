#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Gap affordance implementation using Fuchsia-Controller."""

from honeydew.affordances.fuchsia_controller.bluetooth import bluetooth_common
from honeydew.interfaces.affordances.bluetooth.profiles import bluetooth_gap


class BluetoothGap(bluetooth_common.BluetoothCommon,
                   bluetooth_gap.BluetoothGap):
    """BluetoothGap affordance implementation using Fuchsia-Controller."""
