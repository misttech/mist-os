# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for location affordance."""

import abc


class Location(abc.ABC):
    """Abstract base class for Location affordance."""

    # List all the public methods
    @abc.abstractmethod
    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewLocationError: Error from location stack
            TypeError: Invalid region_code format
        """
