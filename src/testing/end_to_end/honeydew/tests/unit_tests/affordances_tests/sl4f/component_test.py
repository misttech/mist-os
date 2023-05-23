#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.component.py."""

from typing import Any, Dict
import unittest
from unittest import mock

from honeydew.affordances.sl4f import component as sl4f_component
from honeydew.transports import sl4f as sl4f_transport
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


# pylint: disable=protected-access
class ComponentSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.component.py."""

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.component_obj = sl4f_component.Component(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj)

    def test_launch(self) -> None:
        """Testcase for Component.launch()"""
        url = "fuchsia-pkg://fuchsia.com/sl4f#meta/sl4f.cmx"
        self.component_obj.launch(url=url)

        self.sl4f_obj.run.assert_called()

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
    def test_list(self, parameterized_dict) -> None:
        """Testcase for Component.list()"""
        return_value: Dict[str, Any] = parameterized_dict["return_value"]
        self.sl4f_obj.run.return_value = return_value

        self.assertEqual(self.component_obj.list(), return_value["result"])

        self.sl4f_obj.run.assert_called()

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
    def test_search(self, parameterized_dict) -> None:
        """Testcase for Component.search()"""
        self.sl4f_obj.run.return_value = parameterized_dict["return_value"]

        name = "sl4f.cmx"
        self.assertEqual(
            self.component_obj.search(name=name),
            parameterized_dict["expected"])

        self.sl4f_obj.run.assert_called()


if __name__ == "__main__":
    unittest.main()
