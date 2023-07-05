#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances.bluetooth import bluetooth_gap


class BluetoothGap(bluetooth_gap.BluetoothGap):
    """BluetoothGap affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.
        """
        raise NotImplementedError

    def set_discoverable(self, discoverable: bool) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.
        """
        raise NotImplementedError
