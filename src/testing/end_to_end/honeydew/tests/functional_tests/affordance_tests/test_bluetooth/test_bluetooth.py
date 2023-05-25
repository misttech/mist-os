#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Bluetooth affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import bluetooth_capable_device
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class BluetoothAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Bluetooth affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_is_device_bluetooth_capable(self) -> None:
        """Test case to make sure DUT is a Bluetooth capable device"""
        asserts.assert_is_instance(
            self.device, bluetooth_capable_device.BluetoothCapableDevice)

    def test_request_discovery(self) -> None:
        """Test case for bluetooth.request_discovery()"""

        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.bluetooth.request_discovery(True)
            return

        self.device.bluetooth.request_discovery(True)


if __name__ == "__main__":
    test_runner.main()
