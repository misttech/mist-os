# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.user_input.py."""

import unittest
from unittest import mock

import fidl.fuchsia_math as f_math
import fidl.fuchsia_ui_test_input as f_test_input

from honeydew import errors
from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.affordances.ui.user_input import user_input_using_fc
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing import custom_types


# pylint: disable=protected-access
class UserInputFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.ui.user_input.py."""

    def setUp(self) -> None:
        super().setUp()
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController
        )
        self.ffx_transport_obj = mock.MagicMock(spec=ffx_transport.FFX)

    def test_no_virtual_device_support_raise_error(self) -> None:
        """Test for user_input_using_fc.UserInputUsingFc() method raise error without virtual device
        support."""

        with self.assertRaises(errors.NotSupportedError):
            user_input_using_fc.UserInputUsingFc(
                device_name="fuchsia-emulator",
                fuchsia_controller=self.fc_transport_obj,
                ffx_transport=self.ffx_transport_obj,
            )

        self.ffx_transport_obj.run.assert_called_once_with(
            ["component", "list"]
        )

    def test_user_input_no_raise(self) -> None:
        """Test for user_input_using_fc.UserInputUsingFc() method not raise error with virtual
        device support."""
        self.ffx_transport_obj.run.return_value = (
            user_input_using_fc._INPUT_HELPER_COMPONENT
        )
        user_input_using_fc.UserInputUsingFc(
            device_name="fuchsia-emulator",
            fuchsia_controller=self.fc_transport_obj,
            ffx_transport=self.ffx_transport_obj,
        )

    def user_input(self) -> user_input_using_fc.UserInputUsingFc:
        self.ffx_transport_obj.run.return_value = (
            user_input_using_fc._INPUT_HELPER_COMPONENT
        )
        return user_input_using_fc.UserInputUsingFc(
            device_name="fuchsia-emulator",
            fuchsia_controller=self.fc_transport_obj,
            ffx_transport=self.ffx_transport_obj,
        )

    @mock.patch.object(
        f_test_input.Registry.Client,
        "register_touch_screen",
    )
    def test_create_touch_device(self, register_touch_screen) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.create_touch_device() method."""

        touch_device = self.user_input().create_touch_device()

        self.fc_transport_obj.connect_device_proxy.assert_called_once_with(
            custom_types.FidlEndpoint(
                "/core/ui", "fuchsia.ui.test.input.Registry"
            )
        )

        register_touch_screen.assert_called_once()

        self.assertIsNotNone(touch_device._touch_screen_proxy)  # type: ignore[attr-defined]

    def test_tap_only_required(self) -> None:
        """Test for UserInput.tap() method with only required params."""

        touch_device = self.user_input().create_touch_device()
        touch_device._touch_screen_proxy = mock.MagicMock()  # type: ignore[attr-defined]

        touch_device.tap(location=ui_custom_types.Coordinate(x=1, y=2))
        touch_device._touch_screen_proxy.simulate_tap.assert_called_once_with(  # type: ignore[attr-defined]
            tap_location=f_math.Vec(x=1, y=2)
        )

    def test_tap_all_params(self) -> None:
        """Test for UserInput.tap() method with all params."""

        touch_device = self.user_input().create_touch_device(
            touch_screen_size=ui_custom_types.Size(width=3, height=4),
        )
        touch_device._touch_screen_proxy = mock.MagicMock()  # type: ignore[attr-defined]

        touch_device.tap(
            location=ui_custom_types.Coordinate(x=1, y=2),
            tap_event_count=3,
            duration_ms=6,
        )

        touch_device._touch_screen_proxy.simulate_tap.assert_has_calls(  # type: ignore[attr-defined]
            [
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
            ]
        )
