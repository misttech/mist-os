#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.user_input.py."""

import unittest
from unittest import mock

from honeydew.affordances.sl4f.ui import user_input as sl4f_user_input
from honeydew.interfaces.affordances.ui import custom_types
from honeydew.interfaces.affordances.ui import user_input
from honeydew.transports import sl4f as sl4f_transport


# pylint: disable=protected-access
class UserInputSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.ui.user_input.py."""

    def setUp(self) -> None:
        super().setUp()
        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.user_input_obj = sl4f_user_input.UserInput(sl4f=self.sl4f_obj)

    def test_tap_only_required(self) -> None:
        """Test for UserInput.tap() method with only required params."""
        self.user_input_obj.tap(location=custom_types.Coordinate(x=1, y=2))
        self.sl4f_obj.run.assert_called_once_with(
            method="input_facade.Tap",
            params={
                "x": 1,
                "y": 2,
                "width": user_input.DEFAULTS["TOUCH_SCREEN_SIZE"].width,
                "height": user_input.DEFAULTS["TOUCH_SCREEN_SIZE"].height,
                "tap_event_count": user_input.DEFAULTS["TAP_EVENT_COUNT"],
                "duration": user_input.DEFAULTS["DURATION"]
            })

    def test_tap_all_params(self) -> None:
        """Test for UserInput.tap() method with all params."""
        self.user_input_obj.tap(
            location=custom_types.Coordinate(x=1, y=2),
            touch_screen_size=custom_types.Size(width=3, height=4),
            tap_event_count=5,
            duration=6)
        self.sl4f_obj.run.assert_called_once_with(
            method="input_facade.Tap",
            params={
                "x": 1,
                "y": 2,
                "width": 3,
                "height": 4,
                "tap_event_count": 5,
                "duration": 6
            })
