# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.user_input.py."""

import unittest
from unittest import mock

import fidl_fuchsia_math as f_math
import fidl_fuchsia_ui_test_input as f_test_input

from honeydew import errors
from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.affordances.ui.user_input import user_input_using_fc
from honeydew.transports.ffx import ffx as ffx_transport
from honeydew.transports.fuchsia_controller import (
    fuchsia_controller as fc_transport,
)
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
        f_test_input.RegistryClient,
        "register_touch_screen",
        new_callable=mock.AsyncMock,
        return_value=None,
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

    @mock.patch.object(
        f_test_input.TouchScreenClient,
        "simulate_tap",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_touch_screen",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_tap_only_required(self, unused_register_touch_screen, simulate_tap) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.tap() method with only required params."""

        touch_device = self.user_input().create_touch_device()
        touch_device.tap(location=ui_custom_types.Coordinate(x=1, y=2))
        simulate_tap.assert_called_once_with(tap_location=f_math.Vec(x=1, y=2))

    @mock.patch.object(
        f_test_input.TouchScreenClient,
        "simulate_tap",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_touch_screen",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_tap_all_params(self, unused_register_touch_screen, simulate_tap) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.tap() method with all params."""

        touch_device = self.user_input().create_touch_device(
            touch_screen_size=ui_custom_types.Size(width=3, height=4),
        )
        touch_device.tap(
            location=ui_custom_types.Coordinate(x=1, y=2),
            tap_event_count=3,
            duration_ms=6,
        )
        simulate_tap.assert_has_calls(
            [
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
                mock.call(tap_location=f_math.Vec(x=1, y=2)),
            ]
        )

    @mock.patch.object(
        f_test_input.TouchScreenClient,
        "simulate_swipe",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_touch_screen",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_swipe(self, unused_register_touch_screen, simulate_swipe) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.swipe() method."""

        touch_device = self.user_input().create_touch_device(
            touch_screen_size=ui_custom_types.Size(width=4, height=5),
        )
        touch_device.swipe(
            start_location=ui_custom_types.Coordinate(x=1, y=2),
            end_location=ui_custom_types.Coordinate(x=3, y=4),
            move_event_count=2,
        )
        simulate_swipe.assert_has_calls(
            [
                mock.call(
                    start_location=f_math.Vec(x=1, y=2),
                    end_location=f_math.Vec(x=3, y=4),
                    move_event_count=2,
                    duration=0,
                ),
            ]
        )

    @mock.patch.object(
        f_test_input.TouchScreenClient,
        "simulate_swipe",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_touch_screen",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_swipe_with_duration(self, unused_register_touch_screen, simulate_swipe) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.swipe() method with duration."""

        touch_device = self.user_input().create_touch_device(
            touch_screen_size=ui_custom_types.Size(width=4, height=5),
        )
        touch_device.swipe(
            start_location=ui_custom_types.Coordinate(x=1, y=2),
            end_location=ui_custom_types.Coordinate(x=3, y=4),
            move_event_count=2,
            duration_ms=100,
        )
        simulate_swipe.assert_has_calls(
            [
                mock.call(
                    start_location=f_math.Vec(x=1, y=2),
                    end_location=f_math.Vec(x=3, y=4),
                    move_event_count=2,
                    duration=100 * 1000000,
                ),
            ]
        )

    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_keyboard",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_create_keyboard_device(self, register_keyboard) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.create_keyboard_device() method."""

        keyboard_device = self.user_input().create_keyboard_device()
        self.fc_transport_obj.connect_device_proxy.assert_called_once_with(
            custom_types.FidlEndpoint(
                "/core/ui", "fuchsia.ui.test.input.Registry"
            )
        )
        register_keyboard.assert_called_once()

        self.assertIsNotNone(keyboard_device._keyboard_proxy)  # type: ignore[attr-defined]

    @mock.patch.object(
        f_test_input.KeyboardClient,
        "simulate_key_press",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_test_input.RegistryClient,
        "register_keyboard",
        new_callable=mock.AsyncMock,
        return_value=None,
    )
    def test_key_press(self, unused_register_keyboard, simulate_key_press) -> None:  # type: ignore[no-untyped-def]
        """Test for UserInput.key_press() method."""

        keyboard_device = self.user_input().create_keyboard_device()
        keyboard_device.key_press(
            key_code=0xFFFF0002,  # Power
        )
        simulate_key_press.assert_has_calls(
            [
                mock.call(
                    key_code=0xFFFF0002,
                ),
            ]
        )
