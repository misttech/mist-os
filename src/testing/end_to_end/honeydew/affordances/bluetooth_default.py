#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth capability default implementation."""

from honeydew.interfaces.affordances import bluetooth
from honeydew.interfaces.transports import sl4f as sl4f_transport

_SL4F_METHODS = {
    "BluetoothRequestDiscovery": "bt_sys_facade.BluetoothRequestDiscovery",
}


# pylint: disable=attribute-defined-outside-init
class BluetoothDefault(bluetooth.Bluetooth):
    """Default implementation for Bluetooth affordance.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: Implementation of SL4F transport.
    """

    def __init__(self, device_name: str, sl4f: sl4f_transport.SL4F) -> None:
        self._name = device_name
        self._sl4f = sl4f

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self._sl4f.send_sl4f_command(
            method=_SL4F_METHODS["BluetoothRequestDiscovery"],
            params={"discovery": discovery})
