#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.__init__.py."""

import os
from typing import Any, Dict, Set, Type
import unittest
from unittest import mock

import honeydew
from honeydew import errors
from honeydew.device_classes.sl4f import \
    generic_fuchsia_device as sl4f_generic_fuchsia_device
from honeydew.device_classes.sl4f import x64 as sl4f_x64
from honeydew.interfaces.device_classes import fuchsia_device
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class InitTests(unittest.TestCase):
    """Unit tests for honeydew.__init__.py."""

    # List all the tests related to public methods in alphabetical order
    @parameterized.expand(
        [
            (
                {
                    "label": "sl4f_transport",
                    "transport": honeydew.transports.TRANSPORT.SL4F,
                },),
            ({
                "label": "transport_arg_set_to_none",
                "transport": None,
            },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        honeydew.sl4f_generic_fuchsia_device.fuchsia_device.ffx_transport.FFX,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        honeydew.sl4f_generic_fuchsia_device.fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        honeydew.sl4f_generic_fuchsia_device.fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True)
    @mock.patch.object(
        honeydew.sl4f_generic_fuchsia_device.fuchsia_device.ssh_transport.SSH,
        "check_connection",
        autospec=True)
    @mock.patch(
        "honeydew._get_device_class",
        return_value=sl4f_generic_fuchsia_device.GenericFuchsiaDevice,
        autospec=True)
    def test_create_device_return_sl4f_default_device(
            self, parameterized_dict, mock_get_device_class,
            mock_ssh_check_connection, mock_sl4f_start_server,
            mock_sl4f_check_connection, mock_ffx_check_connection) -> None:
        """Test case for honeydew.create_device() where it returns SL4F based
        default fuchsia device object."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-emulator",
                ssh_private_key="/tmp/pkey",
                transport=parameterized_dict["transport"]),
            sl4f_generic_fuchsia_device.GenericFuchsiaDevice)

        mock_get_device_class.assert_called()
        mock_ssh_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "sl4f_transport",
                    "transport": honeydew.transports.TRANSPORT.SL4F,
                },),
            ({
                "label": "transport_arg_set_to_none",
                "transport": None,
            },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        honeydew.device_classes.sl4f.x64.fuchsia_device.ffx_transport.FFX,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        honeydew.device_classes.sl4f.x64.fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        honeydew.device_classes.sl4f.x64.fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True)
    @mock.patch.object(
        honeydew.device_classes.sl4f.x64.fuchsia_device.ssh_transport.SSH,
        "check_connection",
        autospec=True)
    @mock.patch(
        "honeydew._get_device_class", return_value=sl4f_x64.X64, autospec=True)
    def test_create_device_return_sl4f_specific_device(
            self, parameterized_dict, mock_get_device_class,
            mock_ssh_check_connection, mock_sl4f_start_server,
            mock_sl4f_check_connection, mock_ffx_check_connection) -> None:
        """Test case for honeydew.create_device() where it returns a SL4F based
         specific fuchsia device object."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-1234",
                ssh_private_key="/tmp/pkey",
                transport=parameterized_dict["transport"]), sl4f_x64.X64)

        mock_get_device_class.assert_called()
        mock_ssh_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    @mock.patch(
        "honeydew._get_device_class",
        side_effect=RuntimeError("mock runtime error"),
        autospec=True)
    def test_create_device_fuchsia_device_error_exception(
            self, mock_get_device_class) -> None:
        """Test case for honeydew.create_device() raising FuchsiaDeviceError
        exception."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            honeydew.create_device(device_name="fuchsia-1234")

        mock_get_device_class.assert_called()

    @mock.patch(
        "honeydew._get_device_class",
        return_value=sl4f_generic_fuchsia_device.GenericFuchsiaDevice,
        autospec=True)
    def test_get_all_affordances_for_sl4f_based_device(
            self, mock_get_device_class) -> None:
        """Test case for honeydew.get_all_affordances() for a SL4F based
        device."""
        expected_affordances: list[str] = ["bluetooth", "component", "tracing"]

        self.assertEqual(
            honeydew.get_all_affordances(
                device_name="fuchsia-emulator",
                transport=honeydew.transports.TRANSPORT.SL4F),
            expected_affordances)

        mock_get_device_class.assert_called_once()

    def test_get_device_classes(self) -> None:
        """Test case for honeydew.get_device_classes()."""
        device_classes_path: str = os.path.dirname(
            honeydew.device_classes.__file__)
        device_classes_module: str = honeydew._DEVICE_CLASSES_MODULE
        expected_device_classes: Set[Type[fuchsia_device.FuchsiaDevice]] = {
            honeydew.device_classes.sl4f.fuchsia_device.FuchsiaDevice,
            honeydew.device_classes.sl4f.generic_fuchsia_device.
            GenericFuchsiaDevice,
            honeydew.device_classes.sl4f.x64.X64,
        }
        self.assertEqual(
            honeydew.get_device_classes(
                device_classes_path, device_classes_module),
            expected_device_classes)

    @parameterized.expand(
        [
            ({
                "label": "empty_set",
                "fuchsia_device_classes": set(),
            },),
            ({
                "label": "valid_int_set",
                "fuchsia_device_classes": {4, 5},
            },),
            ({
                "label": "valid_int_list",
                "fuchsia_device_classes": [1, 2],
            },),
        ],
        name_func=_custom_test_name_func)
    def test_register_device_classes(self, parameterized_dict) -> None:
        """Test case for honeydew.register_device_classes()."""
        fuchsia_device_classes: Any = parameterized_dict[
            "fuchsia_device_classes"]
        honeydew.register_device_classes(
            fuchsia_device_classes=fuchsia_device_classes)
        self.assertTrue(
            set(fuchsia_device_classes).issubset(
                honeydew._REGISTERED_DEVICE_CLASSES))

    # List all the tests related to private methods in alphabetical order
    @mock.patch(
        "honeydew.get_device_classes",
        return_value={sl4f_x64.X64},
        autospec=True)
    def test_get_all_register_device_classes(
            self, mock_get_device_classes) -> None:
        """Test case for honeydew._get_all_register_device_classes()."""
        self.assertEqual(
            honeydew._get_all_register_device_classes(), {sl4f_x64.X64})
        mock_get_device_classes.assert_called_once()

    @mock.patch.object(
        honeydew.ffx_transport.FFX,
        "get_target_type",
        return_value="qemu-x64",
        autospec=True)
    def test_get_device_class_return_default_sl4f_device(
            self, mock_get_target_type) -> None:
        """Test case for honeydew.create_device() where it returns generic
        Sl4F based fuchsia device class implementation."""
        expected_device_class: Type[fuchsia_device.FuchsiaDevice] = \
            sl4f_generic_fuchsia_device.GenericFuchsiaDevice

        self.assertEqual(
            honeydew._get_device_class(
                device_name="fuchsia-emulator",
                transport=honeydew.transports.TRANSPORT.SL4F),
            expected_device_class)

        mock_get_target_type.assert_called()

    @mock.patch(
        "honeydew._get_all_register_device_classes",
        return_value={sl4f_x64.X64},
        autospec=True)
    @mock.patch.object(
        honeydew.ffx_transport.FFX,
        "get_target_type",
        return_value="x64",
        autospec=True)
    def test_get_device_class_return_specific_sl4f_device(
            self, mock_get_target_type,
            mock_get_all_register_device_classes) -> None:
        """Test case for honeydew._get_device_class() where it returns a
        specific SL4F based fuchsia device class implementation."""
        expected_device_class: Type[fuchsia_device.FuchsiaDevice] = sl4f_x64.X64

        self.assertEqual(
            honeydew._get_device_class(
                device_name="fuchsia-emulator",
                transport=honeydew.transports.TRANSPORT.SL4F),
            expected_device_class)

        mock_get_target_type.assert_called()
        mock_get_all_register_device_classes.assert_called_once()

    @mock.patch.object(
        honeydew.ffx_transport.FFX,
        "get_target_type",
        return_value="something",
        autospec=True)
    def test_get_device_class_raises_run_time_error(
            self, mock_get_target_type) -> None:
        """Test case for honeydew._get_device_class() raising RunTimeError
        exception."""
        with self.assertRaises(RuntimeError):
            honeydew._get_device_class(
                device_name="fuchsia-emulator",
                transport=honeydew.transports.TRANSPORT.FUCHSIA_CONTROLLER)

        mock_get_target_type.assert_called()


if __name__ == "__main__":
    unittest.main()
