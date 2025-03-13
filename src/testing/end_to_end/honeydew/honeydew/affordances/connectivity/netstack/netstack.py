# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for netstack affordance."""

import abc

from honeydew.affordances.connectivity.netstack.types import InterfaceProperties


class Netstack(abc.ABC):
    """Abstract base class for Netstack affordance."""

    # List all the public methods
    @abc.abstractmethod
    def list_interfaces(self) -> list[InterfaceProperties]:
        """List interfaces.

        Returns:
            Information on all interfaces on the device.

        Raises:
            HoneydewNetstackError: Error from the netstack.
        """
