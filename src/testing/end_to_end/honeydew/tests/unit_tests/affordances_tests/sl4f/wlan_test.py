#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.wlan.py."""

import unittest
from typing import Any
from unittest import mock

from parameterized import parameterized

from honeydew.affordances.sl4f.wlan import wlan as sl4f_wlan
from honeydew.transports import sl4f as sl4f_transport


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class WlanSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.wlan.wlan.py."""

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.wlan_obj = sl4f_wlan.Wlan(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj
        )
        self.sl4f_obj.reset_mock()

    @parameterized.expand(
        [
            (
                {
                    "label": "success_case",
                    "return_value": {
                        "result": [12345678],
                    },
                    "expected_value": [12345678],
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_get_phy_ids(self, parameterized_dict) -> None:
        """Test for Wlan.get_phy_ids()."""
        self.sl4f_obj.run.return_value = parameterized_dict["return_value"]

        self.assertEqual(
            self.wlan_obj.get_phy_ids(),
            parameterized_dict["expected_value"],
        )
        self.sl4f_obj.run.assert_called()


if __name__ == "__main__":
    unittest.main()
