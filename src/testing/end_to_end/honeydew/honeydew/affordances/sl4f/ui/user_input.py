# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""UserInput affordance implementation using SL4F."""

from typing import Any

from honeydew.interfaces.affordances.ui import user_input
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing import ui as ui_custom_types

_SL4F_METHODS: dict[str, str] = {
    "Tap": "input_facade.Tap",
}


# TODO(b/335305248): Remove sl4f impl.
class TouchDevice(user_input.TouchDevice):
    """Virtual TouchDevice for testing using SL4F.

    Args:
        sl4f: SL4F transport.
        touch_screen_size: resolution of the touch screen.
    """

    def __init__(
        self, sl4f: sl4f_transport.SL4F, touch_screen_size: ui_custom_types.Size
    ) -> None:
        self._sl4f: sl4f_transport.SL4F = sl4f
        self._touch_screen_size = touch_screen_size

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
        """

        method_params: dict[str, Any] = {
            "x": location.x,
            "y": location.y,
            "width": self._touch_screen_size.width,
            "height": self._touch_screen_size.height,
            "tap_event_count": tap_event_count,
            "duration": duration_ms,
        }

        self._sl4f.run(method=_SL4F_METHODS["Tap"], params=method_params)


class UserInput(user_input.UserInput):
    """UserInput affordance implementation using SL4F.

    Args:
        sl4f: SL4F transport.
    """

    def __init__(self, sl4f: sl4f_transport.SL4F) -> None:
        self._sl4f: sl4f_transport.SL4F = sl4f

    def create_touch_device(
        self,
        touch_screen_size: ui_custom_types.Size = user_input.DEFAULTS[
            "TOUCH_SCREEN_SIZE"
        ],
    ) -> user_input.TouchDevice:
        """Create a virtual touch device for testing touch input.

        Args:
            touch_screen_size: resolution of the touch screen, defaults to
                1000 x 1000.
        """
        return TouchDevice(sl4f=self._sl4f, touch_screen_size=touch_screen_size)
