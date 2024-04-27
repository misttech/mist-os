# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for UserInput affordance."""

import abc
from typing import Any

from honeydew.typing import ui as ui_custom_types

DEFAULTS: dict[str, Any] = {
    "TOUCH_SCREEN_SIZE": ui_custom_types.Size(width=1000, height=1000),
    "TAP_EVENT_COUNT": 1,
    "DURATION_MS": 300,
}


class TouchDevice(abc.ABC):
    """Abstract base class for UserInput Touch."""

    @abc.abstractmethod
    def tap(
        self,
        location: ui_custom_types.Coordinate,
        tap_event_count: int = DEFAULTS["TAP_EVENT_COUNT"],
        duration_ms: int = DEFAULTS["DURATION_MS"],
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


class UserInput(abc.ABC):
    """Abstract base class for UserInput affordance."""

    @abc.abstractmethod
    def create_touch_device(
        self,
        touch_screen_size: ui_custom_types.Size = DEFAULTS["TOUCH_SCREEN_SIZE"],
    ) -> TouchDevice:
        """Create a virtual touch device for testing touch input.

        Args:
            touch_screen_size: resolution of the touch screen, defaults to
                1000 x 1000.
        """
