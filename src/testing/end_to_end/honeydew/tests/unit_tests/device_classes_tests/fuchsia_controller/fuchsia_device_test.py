#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for
honeydew.device_classes.fuchsia_controller.fuchsia_device.py."""

import tempfile
from typing import Any, Dict
import unittest
from unittest import mock

from honeydew import custom_types
from honeydew.device_classes.fuchsia_controller import fuchsia_device
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import \
    fuchsia_device as fuchsia_device_interface
from honeydew.interfaces.device_classes import transports_capable

_INPUT_ARGS: Dict[str, Any] = {
    "device_name": "fuchsia-emulator",
    "ssh_private_key": "/tmp/.ssh/pkey",
    "ssh_user": "root",
}

_MOCK_ARGS: Dict[str, Any] = {
    "device_type": "qemu-x64",
}


# pylint: disable=pointless-statement
# pytype: disable=attribute-error
class FuchsiaDeviceFCTests(unittest.TestCase):
    """Unit tests for
    honeydew.device_classes.fuchsia_controller.fuchsia_device.py."""

    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True)
    def setUp(
            self, mock_ssh_check_connection, mock_ffx_check_connection) -> None:
        super().setUp()

        self.fd_obj = fuchsia_device.FuchsiaDevice(
            device_name=_INPUT_ARGS["device_name"],
            ssh_private_key=_INPUT_ARGS["ssh_private_key"])

        self.assertIsInstance(self.fd_obj, fuchsia_device.FuchsiaDevice)
        mock_ssh_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice)

    # List all the tests related to affordances in alphabetical order
    def test_fuchsia_device_is_bluetooth_capable(self) -> None:
        """Test case to make sure fuchsia device is bluetooth capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.BluetoothCapableDevice)

    def test_fuchsia_device_is_component_capable(self) -> None:
        """Test case to make sure fuchsia device is component capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.ComponentCapableDevice)

    def test_fuchsia_device_is_tracing_capable(self) -> None:
        """Test case to make sure fuchsia device is tracing capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.TracingCapableDevice)

    # List all the tests related to transports in alphabetical order
    def test_fuchsia_device_is_ssh_capable(self) -> None:
        """Test case to make sure fuchsia device is SSH capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.SSHCapableDevice)

    def test_fuchsia_device_is_ffx_capable(self) -> None:
        """Test case to make sure fuchsia device is FFX capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.FFXCapableDevice)

    # List all the tests related to static properties in alphabetical order
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "get_target_type",
        return_value=_MOCK_ARGS["device_type"],
        autospec=True)
    def test_device_type(self, mock_ffx_get_target_type) -> None:
        """Testcase for FuchsiaDevice.device_type property"""
        self.assertEqual(self.fd_obj.device_type, _MOCK_ARGS["device_type"])
        mock_ffx_get_target_type.assert_called()

    def test_manufacturer(self) -> None:
        """Testcase for FuchsiaDevice.manufacturer property"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.manufacturer

    def test_model(self) -> None:
        """Testcase for FuchsiaDevice.model property"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.model

    def test_product_name(self) -> None:
        """Testcase for FuchsiaDevice.product_name property"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.product_name

    def test_serial_number(self) -> None:
        """Testcase for FuchsiaDevice.serial_number property"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.serial_number

    # List all the tests related to dynamic properties in alphabetical order
    def test_firmware_version(self) -> None:
        """Testcase for FuchsiaDevice.firmware_version property"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.firmware_version

    # List all the tests related to public methods in alphabetical order
    def test_close(self) -> None:
        """Testcase for FuchsiaDevice.close()"""
        self.fd_obj.close()

    def test_log_message_to_device(self) -> None:
        """Testcase for FuchsiaDevice.log_message_to_device()"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.log_message_to_device(
                level=custom_types.LEVEL.INFO, message="log_message")

    def test_power_cycle(self) -> None:
        """Testcase for FuchsiaDevice.power_cycle()"""
        power_switch = mock.MagicMock(
            spec=fuchsia_device.power_switch_interface.PowerSwitch)

        with self.assertRaises(NotImplementedError):
            self.fd_obj.power_cycle(power_switch=power_switch, outlet=5)

    def test_reboot(self) -> None:
        """Testcase for FuchsiaDevice.reboot()"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.reboot()

    def test_snapshot(self) -> None:
        """Testcase for FuchsiaDevice.snapshot()"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.snapshot(directory="/tmp")

        with tempfile.TemporaryDirectory() as tmpdir:
            with self.assertRaises(NotImplementedError):
                self.fd_obj.snapshot(directory=tmpdir)
