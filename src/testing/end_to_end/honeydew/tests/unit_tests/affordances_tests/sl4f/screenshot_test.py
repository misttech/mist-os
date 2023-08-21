#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.screenshot.py."""

import base64
from typing import Any, Dict
import unittest
from unittest import mock

from honeydew import errors
from honeydew.affordances.sl4f.ui import screenshot as sl4f_screenshot
from honeydew.transports import sl4f as sl4f_transport


# pylint: disable=protected-access
class ScreenshotSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.ui.screenshot.py."""

    def setUp(self) -> None:
        super().setUp()
        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.screenshot_obj = sl4f_screenshot.Screenshot(sl4f=self.sl4f_obj)

    def test_take_screenshot(self) -> None:
        result_value: Dict[str, Any] = {
            "data":
                base64.b64encode("this_is_a_img".encode("utf-8")
                                ).decode("utf-8"),
            "info": {
                "width": 10,
                "height": 11,
            },
        }
        self.sl4f_obj.run.return_value = {"result": result_value}
        img = self.screenshot_obj.take()
        self.sl4f_obj.run.assert_called_once()
        self.assertEqual(img.size.width, result_value["info"]["width"])
        self.assertEqual(img.size.height, result_value["info"]["height"])
        self.assertEqual(img.data, "this_is_a_img".encode("utf-8"))

    def test_take_screenshot_response_unexpected_format(self) -> None:
        self.sl4f_obj.run.return_value = {"result": {}}

        with self.assertRaises(errors.Sl4fError):
            self.screenshot_obj.take()
        self.sl4f_obj.run.assert_called_once()
