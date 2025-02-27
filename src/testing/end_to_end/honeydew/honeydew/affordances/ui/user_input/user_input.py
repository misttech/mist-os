# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for UserInput affordance."""

import abc
from typing import Any

from honeydew.affordances.ui.user_input import types

DEFAULTS: dict[str, Any] = {
    "TOUCH_SCREEN_SIZE": types.Size(width=1000, height=1000),
    "TAP_EVENT_COUNT": 1,
    "TAP_DURATION_MS": 300,
    "SWIPE_DURATION_MS": 0,
}


class TouchDevice(abc.ABC):
    """Abstract base class for UserInput Touch."""

    @abc.abstractmethod
    def tap(
        self,
        location: types.Coordinate,
        tap_event_count: int = DEFAULTS["TAP_EVENT_COUNT"],
        duration_ms: int = DEFAULTS["TAP_DURATION_MS"],
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

    @abc.abstractmethod
    def swipe(
        self,
        start_location: types.Coordinate,
        end_location: types.Coordinate,
        move_event_count: int,
        duration_ms: int = DEFAULTS["SWIPE_DURATION_MS"],
    ) -> None:
        """Instantiates a swipe event sequence that starts at `start_location` and ends at
           `end_location`, with a total number of move events equal to `move_event_count`.

           Events are injected with no explicit delay in between.

        Args:
            start_location: swipe start location in X, Y axis coordinate.

            end_location: swipe end location in X, Y axis coordinate.

            move_event_count: Number of move events.

            duration_ms: Duration of the swipe gesture in milliseconds, defaults to 0.

        Raises:
            UserInputError: if failed swipe operation.
        """


class UserInput(abc.ABC):
    """Abstract base class for UserInput affordance."""

    @abc.abstractmethod
    def create_touch_device(
        self,
        touch_screen_size: types.Size = DEFAULTS["TOUCH_SCREEN_SIZE"],
    ) -> TouchDevice:
        """Create a virtual touch device for testing touch input.

        Args:
            touch_screen_size: resolution of the touch screen, defaults to
                1000 x 1000.

        Raises:
            UserInputError: if failed to create virtual touch device.
        """
