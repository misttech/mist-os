#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.component_default.py."""

import unittest
from unittest import mock

from honeydew.affordances import component_default
from honeydew.interfaces.transports import sl4f
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param):
    """Custom name function method."""
    test_func_name = testcase_func.__name__

    params_dict = param.args[0]
    test_label = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


# pylint: disable=protected-access
class ComponentDefaultTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.component_default.py."""

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f.SL4F)
        self.component_obj = component_default.ComponentDefault(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj)

    def test_launch(self):
        """Testcase for ComponentDefault.launch()"""
        url = "fuchsia-pkg://fuchsia.com/sl4f#meta/sl4f.cmx"
        self.component_obj.launch(url=url)
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=component_default._SL4F_METHODS["Launch"],
            params={"url": url})

    @parameterized.expand(
        [
            (
                {
                    "label": "success_case",
                    "return_value":
                        {
                            "id": "",
                            "result":
                                [
                                    "fuchsia-boot:///#meta/root.cm",
                                    "fuchsia-boot:///#meta/bootstrap.cm"
                                ],
                            "error": None
                        },
                },),
            (
                {
                    "label": "success_case_with_empty_list",
                    "return_value": {
                        "id": "",
                        "result": [],
                        "error": None
                    },
                },),
        ],
        name_func=_custom_test_name_func)
    def test_list(self, parameterized_dict):
        """Testcase for ComponentDefault.list()"""
        return_value = parameterized_dict["return_value"]
        self.sl4f_obj.send_sl4f_command.return_value = return_value

        self.assertEqual(self.component_obj.list(), return_value["result"])
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=component_default._SL4F_METHODS["List"])

    @parameterized.expand(
        [
            (
                {
                    "label": "when_component_found",
                    "return_value":
                        {
                            "id": "",
                            "result": "Success",
                            "error": None
                        },
                    "expected": True,
                },),
            (
                {
                    "label": "when_component_not_found",
                    "return_value":
                        {
                            "id": "",
                            "result": "NotFound",
                            "error": None
                        },
                    "expected": False,
                },),
        ],
        name_func=_custom_test_name_func)
    def test_search(self, parameterized_dict):
        """Testcase for ComponentDefault.search()"""
        self.sl4f_obj.send_sl4f_command.return_value = parameterized_dict[
            "return_value"]

        name = "sl4f.cmx"
        self.assertEqual(
            self.component_obj.search(name=name),
            parameterized_dict["expected"])
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=component_default._SL4F_METHODS["Search"],
            params={"name": name})


if __name__ == "__main__":
    unittest.main()
