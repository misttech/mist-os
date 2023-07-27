# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Tile affordance."""

import abc
from typing import List


class Tile(abc.ABC):
    """Abstract base class for Tile affordance."""

    @abc.abstractmethod
    def add_tile_from_view_provider(self, url: str) -> int:
        """Add Instantiates a component by its URL and adds a tile backed by
           that component's ViewProvider.

        Args:
            url: url of the component

        Returns:
            a key for the tile that can be used for resizing or removing the
            tile, or 0 on failure.
        """

    @abc.abstractmethod
    def remove_tile(self, key: int) -> None:
        """Removes the tile with the given key.

        Args:
            key: the key of the tile
        """

    @abc.abstractmethod
    def list(self) -> List[int]:
        """Return list of tiles

        Returns:
            a list of key of tiles
        """
