#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Component affordance implementation using Fuchsia-Controller."""

from typing import List

from honeydew.interfaces.affordances import component


class Component(component.Component):
    """Component affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def launch(self, url: str) -> None:
        """Launches a (V2) component.

        Args:
            url: url of the component ending in ".cm"
        """
        raise NotImplementedError

    def list(self) -> List[str]:
        """Returns the list of components that are currently running.

        Returns:
            List containing full component URLs that are currently running.

        """
        raise NotImplementedError

    def search(self, name: str) -> bool:
        """Searches for a component under appmgr and returns True if found.

        Args:
            name: name of the component ending in ".cmx" or ".cm"

        Returns:
            True if component is found else False.
        """
        raise NotImplementedError
