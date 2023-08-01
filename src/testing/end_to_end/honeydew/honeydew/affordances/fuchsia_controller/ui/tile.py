#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tile affordance implementation using fuchsia controller."""

from typing import List

from honeydew.interfaces.affordances.ui import tile


class Tile(tile.Tile):
    """Tile affordance implementation using fuchsia controller."""

    def add_tile_from_view_provider(self, url: str) -> int:
        """Instantiates a component by its URL and adds a tile backed by
           that component's ViewProvider.

        Args:
            url: url of the component

        Returns:
            a key for the tile that can be used for resizing or removing the
            tile, or raise an exception on failure.
        """
        raise NotImplementedError

    def remove_tile(self, key: int) -> None:
        """Removes the tile with the given key.

        Args:
            key: the key of the tile
        """
        raise NotImplementedError

    def list(self) -> List[int]:
        """Return list of tiles

        Returns:
            a list of key of tiles
        """
        raise NotImplementedError
