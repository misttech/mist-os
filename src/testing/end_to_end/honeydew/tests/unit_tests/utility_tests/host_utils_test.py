#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.host_utils.py."""

import subprocess
import unittest
from unittest import mock

from honeydew.utils import host_utils
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param):
    """Custom name function method."""
    test_func_name = testcase_func.__name__

    params_dict = param.args[0]
    test_label = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class HostUtilsTests(unittest.TestCase):
    """Unit tests for honeydew.utils.host_utils.py."""

    def setUp(self) -> None:
        super().setUp()

        # Start all the mock patches
        self.mock_check_output = mock.patch(
            "honeydew.utils.host_utils.subprocess.check_output",
            return_value=b"some_output",
            autospec=True).start()

        # Make sure all mock patches are stopped when the test is completed.
        self.addCleanup(mock.patch.stopall)

    @parameterized.expand(
        [
            (
                {
                    "label":
                        "no_optional_params",
                    "ip":
                        "12.34.56.78",
                    "optional_params": {},
                    "expected_cmd_list":
                        ["ping", "-c", "3", "-W", "5", "12.34.56.78"]
                },),
            (
                {
                    "label":
                        "all_optional_params",
                    "ip":
                        "12.34.56.78",
                    "optional_params":
                        {
                            "packet_count": 4,
                            "timeout": 3,
                            "deadline": 5
                        },
                    "expected_cmd_list":
                        [
                            "ping", "-c", "4", "-W", "3", "-w", "5",
                            "12.34.56.78"
                        ]
                },),
            (
                {
                    "label":
                        "no_deadline",
                    "ip":
                        "12.34.56.78",
                    "optional_params": {
                        "timeout": 3,
                        "packet_count": 4
                    },
                    "expected_cmd_list":
                        ["ping", "-c", "4", "-W", "3", "12.34.56.78"]
                },),
        ],
        name_func=_custom_test_name_func)
    def test_is_pingable_success(self, parameterized_dict):
        """Verifies is_pingable succeeds with correct flag parsing."""
        result = host_utils.is_pingable(
            parameterized_dict["ip"], **parameterized_dict["optional_params"])
        self.assertTrue(result)

        self.mock_check_output.assert_called_once_with(
            parameterized_dict["expected_cmd_list"], stderr=subprocess.STDOUT)

    def test_is_pingable_failure(self):
        """Verifies is_pingable fails when expected."""
        self.mock_check_output.side_effect = subprocess.CalledProcessError(
            "some_err", "some_cmd")

        result = host_utils.is_pingable("12.34.56.78")
        self.assertFalse(result)

        self.mock_check_output.assert_called_once_with(
            ["ping", "-c", "3", "-W", "5", "12.34.56.78"],
            stderr=subprocess.STDOUT)

    @parameterized.expand(
        [
            ({
                "label": "valid_ipv4",
                "ip": "12.34.56.78",
                "expected": True
            },),
            (
                {
                    "label": "valid_ipv6",
                    "ip": "2001:db8::ff00:42:8329",
                    "expected": True
                },),
            ({
                "label": "invalid_ip",
                "ip": "12.34.56",
                "expected": False
            },),
            ({
                "label": "empty_ip",
                "ip": "",
                "expected": False
            },),
            ({
                "label": "random_str",
                "ip": "random str",
                "expected": False
            },),
        ],
        name_func=_custom_test_name_func)
    def test_is_pingable_ip_address_format(self, parameterized_dict):
        """Verifies is_pingable check if the ip_address format is valid."""
        result = host_utils.is_pingable(parameterized_dict["ip"])
        self.assertEqual(result, parameterized_dict["expected"])

        if result:
            self.mock_check_output.assert_called_once()
        else:
            self.mock_check_output.assert_not_called()


if __name__ == '__main__':
    unittest.main()
