#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Component affordance."""

import abc
from typing import List


class Component(abc.ABC):
    """Abstract base class for Component affordance."""

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def launch(self, url: str) -> None:
        """Launches a (V2) component.

        Args:
            url: url of the component ending in ".cmx"
        """

    @abc.abstractmethod
    def list(self) -> List[str]:
        """Returns the list of components that are currently running.

        Returns:
            List containing full component URLs that are currently running.
        """

    @abc.abstractmethod
    def search(self, name: str) -> bool:
        """Searches for a component and returns True if found.

        Args:
            name: name of the component ending in ".cm"

        Returns:
            True if component is found else False.
        """
