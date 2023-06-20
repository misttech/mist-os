#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for fuchsia_device.py device class."""

import logging
import os
import tempfile

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts
from mobly import test_runner

from honeydew import custom_types
from honeydew.device_classes.fuchsia_controller import \
    fuchsia_device as fc_fuchsia_device
from honeydew.device_classes.sl4f import fuchsia_device as sl4f_fuchsia_device
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDeviceTests(fuchsia_base_test.FuchsiaBaseTest):
    """FuchsiaDevice tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns device variable with FuchsiaDevice object
              Note - If there are multiple Fuchsia devices listed in mobly
                     testbed then first device will be used.
            * Assigns device_config variable with testbed config associated with
              this device
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_device_instance(self) -> None:
        """Test case to make sure DUT is a FuchsiaDevice"""
        if self._is_fuchsia_controller_based_device(self.device):
            asserts.assert_is_instance(
                self.device, fc_fuchsia_device.FuchsiaDevice)
        else:
            asserts.assert_is_instance(
                self.device, sl4f_fuchsia_device.FuchsiaDevice)

    def test_device_type(self) -> None:
        """Test case for device_type"""
        asserts.assert_equal(
            self.device.device_type,
            self.user_params["expected_values"]["device_type"])

    def test_manufacturer(self) -> None:
        """Test case for manufacturer"""
        asserts.assert_equal(
            self.device.manufacturer,
            self.user_params["expected_values"]["manufacturer"])

    def test_model(self) -> None:
        """Test case for model"""
        asserts.assert_equal(
            self.device.model, self.user_params["expected_values"]["model"])

    def test_product_name(self) -> None:
        """Test case for product_name"""
        asserts.assert_equal(
            self.device.product_name,
            self.user_params["expected_values"]["product_name"])

    def test_serial_number(self) -> None:
        """Test case for serial_number"""
        # Note - Some devices such as FEmu, X64 does not have a serial_number.
        # So do not include "serial_number" in params.yml file if device does
        # not have a serial_number.
        asserts.assert_equal(
            self.device.serial_number,
            self.user_params["expected_values"].get("serial_number"))

    def test_firmware_version(self) -> None:
        """Test case for firmware_version"""
        # Note - If "firmware_version" is specified in "expected_values" in
        # params.yml then compare with it.
        if "firmware_version" in self.user_params["expected_values"]:
            asserts.assert_equal(
                self.device.firmware_version,
                self.user_params["expected_values"]["firmware_version"])
        else:
            asserts.assert_is_instance(self.device.firmware_version, str)

    def test_log_message_to_device(self) -> None:
        """Test case for log_message_to_device()"""
        self.device.log_message_to_device(
            message="This is a test ERROR message",
            level=custom_types.LEVEL.ERROR)

        self.device.log_message_to_device(
            message="This is a test WARNING message",
            level=custom_types.LEVEL.WARNING)

        self.device.log_message_to_device(
            message="This is a test INFO message",
            level=custom_types.LEVEL.INFO)

    def test_reboot(self) -> None:
        """Test case for reboot()"""
        self.device.reboot()

    def test_snapshot(self) -> None:
        """Test case for snapshot()"""
        # TODO(b/286052015): Implement snapshot support.
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.snapshot(directory="/tmp",)
            return

        with tempfile.TemporaryDirectory() as tmpdir:
            self.device.snapshot(directory=tmpdir, snapshot_file="snapshot.zip")
            exists: bool = os.path.exists(f"{tmpdir}/snapshot.zip")
        asserts.assert_true(exists, msg="snapshot failed")


if __name__ == '__main__':
    test_runner.main()
