# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""UserInput affordance implementation using FuchsiaController."""

import time

import fidl.fuchsia_math as f_math
import fidl.fuchsia_ui_test_input as f_test_input
import fuchsia_controller_py as fcp

from honeydew import errors
from honeydew.affordances.ui.user_input import errors as user_input_errors
from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.affordances.ui.user_input import user_input
from honeydew.transports import ffx
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing import custom_types

_INPUT_HELPER_COMPONENT: str = "core/ui/input-helper"


class _FcProxies:
    INPUT_REGISTRY: custom_types.FidlEndpoint = custom_types.FidlEndpoint(
        "/core/ui", "fuchsia.ui.test.input.Registry"
    )


class TouchDevice(user_input.TouchDevice):
    """Virtual TouchDevice for testing using FuchsiaController.

    Args:
        device_name: name of testing device.
        fuchsia_controller: FuchsiaController transport.

    Raises:
        UserInputError: if failed to create virtual touch device.
    """

    def __init__(
        self,
        device_name: str,
        fuchsia_controller: fc_transport.FuchsiaController,
    ) -> None:
        self._device_name = device_name
        channel_server, channel_client = fcp.Channel.create()

        try:
            input_registry_proxy = f_test_input.Registry.Client(
                fuchsia_controller.connect_device_proxy(
                    _FcProxies.INPUT_REGISTRY
                )
            )
            input_registry_proxy.register_touch_screen(
                device=channel_server.take(),
            )
        except fcp.ZxStatus as status:
            raise user_input_errors.UserInputError(
                f"Failed to initialize touch device on {self._device_name}"
            ) from status

        self._touch_screen_proxy = f_test_input.TouchScreen.Client(
            channel_client
        )

    def tap(
        self,
        location: ui_custom_types.Coordinate,
        tap_event_count: int = user_input.DEFAULTS["TAP_EVENT_COUNT"],
        duration_ms: int = user_input.DEFAULTS["DURATION_MS"],
    ) -> None:
        """Instantiates Taps at coordinates (x, y) for a touchscreen with
           default or custom width, height, duration, and tap event counts.

        Args:
            location: tap location in X, Y axis coordinate.

            tap_event_count: Number of tap events to send (`duration` is
                divided over the tap events), defaults to 1.

            duration_ms: Duration of the event(s) in milliseconds, defaults to
                300.

        Raises:
            UserInputError: if failed tap operation.
        """

        try:
            interval: float = duration_ms / tap_event_count

            for _ in range(tap_event_count):
                self._touch_screen_proxy.simulate_tap(
                    tap_location=f_math.Vec(x=location.x, y=location.y)
                )
                time.sleep(interval / 1000)  # Sleep in seconds

        except fcp.ZxStatus as status:
            raise user_input_errors.UserInputError(
                f"tap operation failed on {self._device_name}"
            ) from status


class UserInputUsingFc(user_input.UserInput):
    """UserInput affordance implementation using FuchsiaController.

    Args:
        device_name: name of testing device.
        fuchsia_controller: FuchsiaController transport.

    Raises:
        NotSupportedError: if device does not support virtual input device.
    """

    def __init__(
        self,
        device_name: str,
        fuchsia_controller: fc_transport.FuchsiaController,
        ffx_transport: ffx.FFX,
    ) -> None:
        self._device_name = device_name
        self._fc_transport: fc_transport.FuchsiaController = fuchsia_controller

        # check if the device have component to support virtual devices.
        components = ffx_transport.run(["component", "list"])
        if _INPUT_HELPER_COMPONENT not in components.splitlines():
            raise errors.NotSupportedError(
                f"{_INPUT_HELPER_COMPONENT} is not available in device {device_name}"
            )

    def create_touch_device(
        self,
        touch_screen_size: ui_custom_types.Size = user_input.DEFAULTS[
            "TOUCH_SCREEN_SIZE"
        ],
    ) -> user_input.TouchDevice:
        """Create a virtual touch device for testing touch input.

        Args:
            touch_screen_size: ignore.

        Raises:
            UserInputError: if failed to create virtual touch device.
        """
        return TouchDevice(
            device_name=self._device_name, fuchsia_controller=self._fc_transport
        )
