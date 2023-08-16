# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for UserInput affordance."""

import abc
from typing import Any, Dict

from honeydew.interfaces.affordances.ui import custom_types

DEFAULTS: Dict[str, Any] = {
    "TOUCH_SCREEN_SIZE": custom_types.Size(width=1000, height=1000),
    "TAP_EVENT_COUNT": 1,
    "DURATION": 300,
}


class UserInput(abc.ABC):
    """Abstract base class for UserInput affordance."""

    @abc.abstractmethod
    def tap(
            self,
            location: custom_types.Coordinate,
            touch_screen_size: custom_types.
        Size = DEFAULTS["TOUCH_SCREEN_SIZE"],
            tap_event_count: int = DEFAULTS["TAP_EVENT_COUNT"],
            duration: int = DEFAULTS["DURATION"]) -> None:
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
