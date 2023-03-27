#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for affordances/bluetooth_default.py."""

import logging

from honeydew.interfaces.device_classes import bluetooth_capable_device
from honeydew.mobly_controller import fuchsia_device
from mobly import asserts, base_test, test_runner

_LOGGER = logging.getLogger(__name__)


class BluetoothCapabilityTests(base_test.BaseTestClass):
    """Bluetooth capability tests"""

    def setup_class(self):
        """setup_class is called once before running tests."""
        fuchsia_devices = self.register_controller(fuchsia_device)
        self.device = fuchsia_devices[0]

    def on_fail(self, _):
        """on_fail is called once when a test case fails."""
        if not hasattr(self, "device"):
            return

        try:
            self.device.snapshot(directory=self.current_test_info.output_path)
        except Exception:  # pylint: disable=broad-except
            _LOGGER.warning("Unable to take snapshot")

    def test_is_device_bluetooth_capable(self):
        """Test case to make sure DUT is a Bluetooth capable device"""
        asserts.assert_is_instance(
            self.device, bluetooth_capable_device.BluetoothCapableDevice)

    def test_request_discovery(self):
        """Test case for bluetooth.request_discovery()"""
        self.device.bluetooth.request_discovery(True)


if __name__ == "__main__":
    test_runner.main()
