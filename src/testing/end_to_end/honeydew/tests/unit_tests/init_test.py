#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.__init__.py."""

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

    @mock.patch.object(
        generic_fuchsia_device.fuchsia_device_base.FuchsiaDeviceBase,
        "_start_sl4f_server",
        autospec=True)
    @mock.patch.object(
        honeydew.fuchsia_device.ffx_cli,
        "get_target_address",
        return_value="12.34.56.78",
        autospec=True)
    @mock.patch.object(
        honeydew.ffx_cli,
        "get_target_type",
        return_value="qemu-x64",
        autospec=True)
    def test_create_device_return_default_device(
            self, mock_get_target_type, mock_get_target_address,
            mock_start_sl4f_server):
        """Test case for honeydew.create_device() where it returns default
        fuchsia device object."""
        device_name = "fuchsia-emulator"
        fd_obj = honeydew.create_device(
            device_name=device_name, ssh_pkey="/tmp/pkey")

        self.assertIsInstance(
            fd_obj, generic_fuchsia_device.GenericFuchsiaDevice)

        mock_get_target_type.assert_called_once_with(device_name)
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
        "honeydew._get_all_register_device_classes",
        return_value={x64.X64},
        autospec=True)
    @mock.patch.object(
        honeydew.ffx_cli, "get_target_type", return_value="x64", autospec=True)
    def test_create_device_return_specific_device(
            self, mock_get_target_type, mock_get_all_register_device_classes,
            mock_get_target_address, mock_start_sl4f_server):
        """Test case for honeydew.create_device() where it returns a specific
        fuchsia device object."""
        device_name = "fuchsia-emulator"
        fd_obj = honeydew.create_device(
            device_name=device_name, ssh_pkey="/tmp/pkey")

        self.assertIsInstance(fd_obj, x64.X64)

        mock_get_target_type.assert_called_once_with(device_name)
        mock_get_all_register_device_classes.assert_called_once()
        mock_get_target_address.assert_called_once_with(device_name)
        mock_start_sl4f_server.assert_called_once()

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

    # Note - Testing for honeydew._get_device_classes() and
    # honeydew._get_all_register_device_classes() is covered in
    # test_create_device_return_default_device.
