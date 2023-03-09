#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.__init__.py."""

import os
import unittest
from unittest import mock

import honeydew
from honeydew.device_classes import generic_fuchsia_device, x64
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param):
    """Custom name function method."""
    test_func_name = testcase_func.__name__

    params_dict = param.args[0]
    test_label = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class InitTests(unittest.TestCase):
    """Unit tests for honeydew.__init__.py."""

    # List all the tests related to public methods in alphabetical order
    @mock.patch.object(
        generic_fuchsia_device.fuchsia_device_base.FuchsiaDeviceBase,
        "_start_sl4f_server",
        autospec=True)
    @mock.patch.object(
        honeydew.fuchsia_device.ffx_cli,
        "get_target_address",
        return_value="12.34.56.78",
        autospec=True)
    @mock.patch(
        "honeydew._get_device_class",
        return_value=generic_fuchsia_device.GenericFuchsiaDevice,
        autospec=True)
    def test_create_device_return_default_device(
            self, mock_get_device_class, mock_get_target_address,
            mock_start_sl4f_server):
        """Test case for honeydew.create_device() where it returns default
        fuchsia device object."""
        device_name = "fuchsia-emulator"

        self.assertIsInstance(
            honeydew.create_device(
                device_name=device_name, ssh_private_key="/tmp/pkey"),
            generic_fuchsia_device.GenericFuchsiaDevice)

        mock_get_device_class.assert_called_once_with(device_name)
        mock_get_target_address.assert_called_once_with(device_name)
        mock_start_sl4f_server.assert_called_once()

    @mock.patch.object(
        x64.fuchsia_device_base.FuchsiaDeviceBase,
        "_start_sl4f_server",
        autospec=True)
    @mock.patch.object(
        honeydew.fuchsia_device.ffx_cli,
        "get_target_address",
        return_value="12.34.56.78",
        autospec=True)
    @mock.patch(
        "honeydew._get_device_class", return_value=x64.X64, autospec=True)
    def test_create_device_return_specific_device(
            self, mock_get_device_class, mock_get_target_address,
            mock_start_sl4f_server):
        """Test case for honeydew.create_device() where it returns a specific
        fuchsia device object."""
        device_name = "fuchsia-1234"
        self.assertIsInstance(
            honeydew.create_device(
                device_name=device_name, ssh_private_key="/tmp/pkey"), x64.X64)

        mock_get_device_class.assert_called_once_with(device_name)
        mock_get_target_address.assert_called_once_with(device_name)
        mock_start_sl4f_server.assert_called_once()

    @mock.patch(
        "honeydew._get_device_class",
        return_value=generic_fuchsia_device.GenericFuchsiaDevice,
        autospec=True)
    def test_get_all_affordances(self, mock_get_device_class):
        """Test case for honeydew.get_all_affordances()."""
        device_name = "fuchsia-emulator"
        expected_affordances = ['component']

        self.assertEqual(
            honeydew.get_all_affordances(device_name), expected_affordances)
        mock_get_device_class.assert_called_once()

    def test_get_device_classes(self):
        """Test case for honeydew.get_device_classes()."""
        device_classes_path = os.path.dirname(honeydew.device_classes.__file__)
        device_classes_module = honeydew._DEVICE_CLASSES_MODULE
        expected_device_classes = {
            honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase,
            honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice,
            honeydew.device_classes.x64.X64
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
    def test_register_device_classes(self, parameterized_dict):
        """Test case for honeydew.register_device_classes()."""
        fuchsia_device_classes = parameterized_dict["fuchsia_device_classes"]
        honeydew.register_device_classes(
            fuchsia_device_classes=fuchsia_device_classes)
        self.assertTrue(
            set(fuchsia_device_classes).issubset(
                honeydew._REGISTERED_DEVICE_CLASSES))

    # List all the tests related to private methods in alphabetical order
    @mock.patch.object(
        honeydew.ffx_cli,
        "get_target_type",
        return_value="qemu-x64",
        autospec=True)
    def test_get_device_class_return_default_device(self, mock_get_target_type):
        """Test case for honeydew.create_device() where it returns generic
        fuchsia device class implementation."""
        device_name = "fuchsia-emulator"
        expected_device_class = generic_fuchsia_device.GenericFuchsiaDevice

        self.assertEqual(
            honeydew._get_device_class(device_name=device_name),
            expected_device_class)

        mock_get_target_type.assert_called_once_with(device_name)

    @mock.patch(
        "honeydew.get_device_classes", return_value={x64.X64}, autospec=True)
    def test_get_all_register_device_classes(self, mock_get_device_classes):
        """Test case for honeydew._get_all_register_device_classes()."""
        self.assertEqual(honeydew._get_all_register_device_classes(), {x64.X64})
        mock_get_device_classes.assert_called_once()

    @mock.patch(
        "honeydew._get_all_register_device_classes",
        return_value={x64.X64},
        autospec=True)
    @mock.patch.object(
        honeydew.ffx_cli, "get_target_type", return_value="x64", autospec=True)
    def test_get_device_class_return_specific_device(
            self, mock_get_target_type, mock_get_all_register_device_classes):
        """Test case for honeydew._get_device_class() where it returns a
        specific fuchsia device class implementation."""
        device_name = "fuchsia-emulator"
        expected_device_class = x64.X64

        self.assertEqual(
            honeydew._get_device_class(device_name=device_name),
            expected_device_class)

        mock_get_target_type.assert_called_once_with(device_name)
        mock_get_all_register_device_classes.assert_called_once()
