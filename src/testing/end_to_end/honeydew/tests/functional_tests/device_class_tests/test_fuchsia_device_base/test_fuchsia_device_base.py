#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for device_classes/fuchsia_device_base.py."""

import logging
import os
import tempfile

from honeydew import custom_types
from honeydew.device_classes import fuchsia_device_base
from honeydew.interfaces.device_classes import (
    component_capable_device, fuchsia_device)
from honeydew.mobly_controller import \
    fuchsia_device as fuchsia_device_mobly_controller
from mobly import asserts, base_test, test_runner

_LOGGER = logging.getLogger(__name__)


# pylint: disable=attribute-defined-outside-init
class FuchsiaDeviceBaseTests(base_test.BaseTestClass):
    """FuchsiaDeviceBase tests run using an X64 device"""

    def setup_class(self):
        fuchsia_devices = self.register_controller(
            fuchsia_device_mobly_controller)
        self.device = fuchsia_devices[0]

    def on_fail(self, record):
        if not hasattr(self, "device"):
            return

        try:
            self.device.snapshot(directory=self.current_test_info.output_path)
        except Exception:  # pylint: disable=broad-except
            _LOGGER.warning("Unable to take snapshot")

    def test_device_instance(self):
        """Test case to make sure DUT is a FuchsiaDeviceBase"""
        asserts.assert_is_instance(
            self.device, fuchsia_device_base.FuchsiaDeviceBase)

    def test_device_is_a_fuchsia_device(self):
        """Test case to make sure DUT is a fuchsia device"""
        asserts.assert_is_instance(self.device, fuchsia_device.FuchsiaDevice)

    def test_fuchsia_device_is_component_capable(self):
        """Test case to make sure fuchsia device is a component capable device"""
        asserts.assert_is_instance(
            self.device, component_capable_device.ComponentCapableDevice)

    def test_device_type(self):
        """Test case for serial_number"""
        asserts.assert_equal(
            self.device.device_type,
            self.user_params["expected_values"]["device_type"])

    def test_manufacturer(self):
        """Test case for manufacturer"""
        asserts.assert_equal(
            self.device.manufacturer,
            self.user_params["expected_values"]["manufacturer"])

    def test_model(self):
        """Test case for model"""
        asserts.assert_equal(
            self.device.model, self.user_params["expected_values"]["model"])

    def test_product_name(self):
        """Test case for product_name"""
        asserts.assert_equal(
            self.device.product_name,
            self.user_params["expected_values"]["product_name"])

    def test_serial_number(self):
        """Test case for serial_number"""
        # Note - Some devices such as FEmu, X64 does not have a serial_number.
        # So do not include "serial_number" in params.yml file if device does
        # not have a serial_number.
        asserts.assert_equal(
            self.device.serial_number,
            self.user_params["expected_values"].get("serial_number"))

    def test_firmware_version(self):
        """Test case for firmware_version"""

        # Note - If "firmware_version" is specified in "expected_values" in
        # params.yml then compare with it.
        if "firmware_version" in self.user_params["expected_values"]:
            asserts.assert_equal(
                self.device.firmware_version,
                self.user_params["expected_values"]["firmware_version"])
        else:
            asserts.assert_is_instance(self.device.firmware_version, str)

    def test_log_message_to_device(self):
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

    def test_reboot(self):
        """Test case for reboot()"""
        self.device.reboot()

    def test_snapshot(self):
        """Test case for snapshot()"""
        with tempfile.TemporaryDirectory() as tmpdir:
            self.device.snapshot(directory=tmpdir, snapshot_file="snapshot.zip")
            exists = os.path.exists(f"{tmpdir}/snapshot.zip")
        asserts.assert_true(exists, msg="snapshot failed")


if __name__ == '__main__':
    test_runner.main()
