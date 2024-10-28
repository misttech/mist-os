# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by netstack affordance."""

from __future__ import annotations

import enum
from dataclasses import dataclass
from ipaddress import IPv4Address, IPv6Address

import fidl.fuchsia_net as f_net
import fidl.fuchsia_net_interfaces as f_net_interfaces

from honeydew.affordances.connectivity.wlan.utils.types import MacAddress


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

    mac: MacAddress | None
    """The MAC address of the interface, if there is one."""

    ipv4_addresses: list[IPv4Address]
    """IPv4 addresses currently installed on the interface."""

    ipv6_addresses: list[IPv6Address]
    """IPv6 addresses currently installed on the interface."""

    port_class: PortClass
    """The port class of the interface."""

    @staticmethod
    def from_fidl(
        fidl: f_net_interfaces.Properties, mac: MacAddress | None
    ) -> "InterfaceProperties":
        """Create an InterfaceProperties from the FIDL equivalent.

        `mac` is necessary since fuchsia.net.interface/Properties does not
        include a MAC address. It has to be fetched separately using
        fuchsia.net.root/Interfaces.GetMac()
        """
        ipv4_addresses: list[IPv4Address] = []
        ipv6_addresses: list[IPv6Address] = []

        for address in fidl.addresses:
            subnet = address.addr
            ip = subnet.addr
            if ip.ipv4:
                ipv4_addresses.append(IPv4Address(bytes(ip.ipv4.addr)))
            elif ip.ipv6:
                ipv6_addresses.append(IPv6Address(bytes(ip.ipv6.addr)))
            else:
                raise TypeError(f"Unknown IP address type: {ip}")

        if fidl.port_class.loopback:
            port_class = PortClass.LOOPBACK
        elif fidl.port_class.device:
            port_class = PortClass(fidl.port_class.device)
        else:
            raise TypeError(f"Unknown port_class: {fidl.port_class}")

        return InterfaceProperties(
            id=fidl.id,
            name=fidl.name,
            mac=mac,
            ipv4_addresses=ipv4_addresses,
            ipv6_addresses=ipv6_addresses,
            port_class=port_class,
        )

    def to_fidl(self) -> f_net_interfaces.Properties:
        """Convert to the FIDL equivalent."""
        addresses: list[f_net_interfaces.Address] = []

        for ipv4 in self.ipv4_addresses:
            addr = f_net.IpAddress()
            addr.ipv4 = f_net.Ipv4Address(
                addr=list(ipv4.packed),
            )
            addresses.append(
                f_net_interfaces.Address(
                    addr=f_net.Subnet(
                        addr=addr,
                        prefix_len=0,
                    ),
                    valid_until=None,
                    preferred_lifetime_info=None,
                    assignment_state=None,
                )
            )

        for ipv6 in self.ipv6_addresses:
            addr = f_net.IpAddress()
            addr.ipv6 = f_net.Ipv6Address(
                addr=list(ipv6.packed),
            )
            addresses.append(
                f_net_interfaces.Address(
                    addr=f_net.Subnet(
                        addr=addr,
                        prefix_len=0,
                    ),
                    valid_until=None,
                    preferred_lifetime_info=None,
                    assignment_state=None,
                )
            )

        port_class = f_net_interfaces.PortClass()
        if self.port_class is PortClass.LOOPBACK:
            port_class.loopback = f_net_interfaces.Empty()
        else:
            port_class.device = self.port_class.value

        return f_net_interfaces.Properties(
            id=self.id,
            addresses=addresses,
            online=None,
            device_class=None,
            has_default_ipv4_route=None,
            has_default_ipv6_route=None,
            name=self.name,
            port_class=port_class,
        )


class PortClass(enum.IntEnum):
    """Network port class.

    Loosely mirrors fuchsia.hardware.network/PortClass.
    """

    LOOPBACK = 0  # not part of fuchsia.hardware.network/PortClass
    ETHERNET = 1
    WLAN_CLIENT = 2
    PPP = 3
    BRIDGE = 4
    WLAN_AP = 5
    VIRTUAL = 6
    LOWPAN = 7
