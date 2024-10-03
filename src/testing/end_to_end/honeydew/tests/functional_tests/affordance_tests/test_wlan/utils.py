# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities for WLAN functional tests."""

import time

from mobly import signals

from honeydew.interfaces.affordances.netstack import Netstack
from honeydew.typing.netstack import InterfaceProperties, PortClass

# Time to wait for a WLAN interface to become available.
INTERFACE_TIMEOUT = 30


def wait_for_interface(netstack: Netstack, port_class: PortClass) -> None:
    """Wait for an interface to become available.

    Args:
        netstack: Netstack affordance
        port_class: Desired type of interface

    Raises:
        TestAbortClass: Desired interface does not exist
    """
    interfaces: list[InterfaceProperties] = []
    end_time = time.time() + INTERFACE_TIMEOUT
    while time.time() < end_time:
        interfaces = netstack.list_interfaces()
        for interface in interfaces:
            if interface.port_class is port_class:
                return
        time.sleep(1)  # Prevent denial-of-service
    raise signals.TestAbortClass(
        f"Expected presence of a {port_class.name} interface, got {interfaces}"
    )
