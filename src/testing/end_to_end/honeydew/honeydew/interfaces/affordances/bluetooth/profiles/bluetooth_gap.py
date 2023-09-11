#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Bluetooth Gap Profile affordance."""

from honeydew.interfaces.affordances.bluetooth import bluetooth_common


class BluetoothGap(bluetooth_common.BluetoothCommon):
    """Abstract base class for BluetoothGap Profile affordance."""
