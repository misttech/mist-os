# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by netstack affordance."""

from __future__ import annotations

from dataclasses import dataclass
from ipaddress import IPv4Address, IPv6Address


# Not all fields of fuchsia.net.interfaces/Properties are added below.
#
# TODO(http://b/355718339): Replace with statically generated FIDL Python type
# once available. See fuchsia.net.interfaces/Properties to view additional
# fields that may be implemented.
@dataclass
class InterfaceProperties:
    """Properties of a network interface."""

    id: int
    """An opaque identifier for the interface."""

    name: str
    """The name of the interface."""

    ipv4_addresses: list[IPv4Address]
    """IPv4 addresses currently installed on the interface."""

    ipv6_addresses: list[IPv6Address]
    """IPv6 addresses currently installed on the interface."""
