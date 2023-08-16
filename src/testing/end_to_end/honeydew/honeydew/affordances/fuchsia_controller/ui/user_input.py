# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""UserInput affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances.ui import custom_types
from honeydew.interfaces.affordances.ui import user_input


class UserInput(user_input.UserInput):
    """UserInput affordance implementation using Fuchsia-Controller."""

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
        raise NotImplementedError
