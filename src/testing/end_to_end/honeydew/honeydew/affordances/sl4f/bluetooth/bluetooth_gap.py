#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth affordance implementation using SL4F."""

from typing import Dict

from honeydew.interfaces.affordances.bluetooth import bluetooth_gap
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import sl4f as sl4f_transport

_SL4F_METHODS: Dict[str, str] = {
    "BluetoothSetDiscoverable": "bt_sys_facade.BluetoothSetDiscoverable",
    "BluetoothInitSys": "bt_sys_facade.BluetoothInitSys",
    "BluetoothRequestDiscovery": "bt_sys_facade.BluetoothRequestDiscovery",
}


class BluetoothGap(bluetooth_gap.BluetoothGap):
    """BluetoothGap affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """

    def __init__(
            self, device_name: str, sl4f: sl4f_transport.SL4F,
            reboot_affordance: affordances_capable.RebootCapableDevice) -> None:
        self._name: str = device_name
        self._sl4f: sl4f_transport.SL4F = sl4f
        self._reboot_affordance: affordances_capable.RebootCapableDevice = \
            reboot_affordance

        # `sys_init` need to be called on every device bootup
        self._reboot_affordance.register_for_on_device_boot(fn=self.sys_init)

        # Initialize the bluetooth stack
        self.sys_init()

    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Note: This method is called automatically:
            1. During this class initialization
            2. After the device reboot

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self._sl4f.run(method=_SL4F_METHODS["BluetoothInitSys"])

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self._sl4f.run(
            method=_SL4F_METHODS["BluetoothRequestDiscovery"],
            params={"discovery": discovery})

    def set_discoverable(self, discoverable: bool) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self._sl4f.run(
            method=_SL4F_METHODS["BluetoothSetDiscoverable"],
            params={"discoverable": discoverable})
