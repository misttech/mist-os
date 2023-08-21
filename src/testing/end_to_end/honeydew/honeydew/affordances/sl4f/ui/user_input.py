# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""UserInput affordance implementation using SL4F."""

from typing import Any, Dict

from honeydew.interfaces.affordances.ui import custom_types
from honeydew.interfaces.affordances.ui import user_input
from honeydew.transports import sl4f as sl4f_transport

_SL4F_METHODS: Dict[str, str] = {
    "Tap": "input_facade.Tap",
}


class UserInput(user_input.UserInput):
    """UserInput affordance implementation using SL4F.

    Args:
        sl4f: SL4F transport.
    """

    def __init__(self, sl4f: sl4f_transport.SL4F) -> None:
        self._sl4f: sl4f_transport.SL4F = sl4f

    def tap(
            self,
            location: custom_types.Coordinate,
            touch_screen_size: custom_types.Size = user_input.
        DEFAULTS["TOUCH_SCREEN_SIZE"],
            tap_event_count: int = user_input.DEFAULTS["TAP_EVENT_COUNT"],
            duration: int = user_input.DEFAULTS["DURATION"]) -> None:
        """Instantiates Taps at coordinates (x, y) for a touchscreen with
           default or custom width, height, duration, and tap event counts.

        Args:
            location: tap location in X, Y axis coordinate.

            touch_screen_size: resolution of the touch panel, defaults to
                1000 x 1000.

            tap_event_count: Number of tap events to send (`duration` is
                divided over the tap events), defaults to 1.

            duration: Duration of the event(s) in milliseconds, defaults to
                300.
        """

        method_params: Dict[str, Any] = {
            "x": location.x,
            "y": location.y,
            "width": touch_screen_size.width,
            "height": touch_screen_size.height,
            "tap_event_count": tap_event_count,
            "duration": duration,
        }

        self._sl4f.run(method=_SL4F_METHODS["Tap"], params=method_params)
