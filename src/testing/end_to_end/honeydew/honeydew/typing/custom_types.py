# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom data types."""

from __future__ import annotations

import enum
import ipaddress
from dataclasses import dataclass
from typing import TypeVar

AnyString = TypeVar("AnyString", str, bytes)


class LEVEL(enum.StrEnum):
    """Logging level that need to specified to log a message onto device"""

    INFO = "Info"
    WARNING = "Warning"
    ERROR = "Error"


@dataclass(frozen=True)
class IpPort:
    """Dataclass that holds IP Address and Port

    Args:
        ip: Ip Address
        port: Port Number
    """

    ip: ipaddress.IPv4Address | ipaddress.IPv6Address
    port: int | None

    def __post_init__(self) -> None:
        """Validates ip and port args.

        Raises:
            ValueError
        """
        if self.port is not None and self.port < 1:
            raise ValueError(
                f"port number: {self.port} was not a positive integer"
            )

    def __str__(self) -> str:
        host: str = f"{self.ip}"
        if isinstance(self.ip, ipaddress.IPv6Address):
            host = f"[{host}]"
        if self.port:
            return f"{host}:{self.port}"
        else:
            return f"{host}"

    @staticmethod
    def create_using_ip_and_port(ip_port: str) -> IpPort:
        """Factory method to create IpPort object using str that has both ip
        and port values.

        Args:
            ip_port: IP address and port of the fuchsia device. This is of
                     one the following formats:
                        {ipv4_address}:{port}
                        [{ipv6_address}]:{port}
                        {ipv6_address}:{port}

        Returns:
            A valid IpPort

        Raises:
          ValueError
        """
        try:
            # If we have something of form
            #     192.168.1.1:8888 ==> ["192.168.1.1", "8888"]
            # If we have something of form
            #     [::1]:8888 ==> ["[::1]", "8888"]
            arr: list[str] = ip_port.rsplit(":", 1)
            if len(arr) != 1 and len(arr) != 2:
                raise ValueError(
                    f"Value: {ip_port} was not a valid IpPort (needs "
                    f"IP Address and optional Port)"
                )
            addr_part: str = arr[0]
            # Remove [] that might be surrounding an IPv6 address
            addr_part = addr_part.replace("[", "").replace("]", "")

            port = None
            if len(arr) == 2:
                port_part: str = arr[1]
                port = int(port_part)
                if port < 1:
                    raise ValueError(
                        f"For IpPort: {ip_port}, port number: {port} was "
                        f"not a positive integer)"
                    )

            return IpPort(ipaddress.ip_address(addr_part), port)
        except ValueError as e:
            raise e

    @staticmethod
    def create_using_ip(ip: str) -> IpPort:
        """Factory method to create IpPort object using str that has ip address.

        Args:
            ip: IP address and port of the fuchsia device. This is of
                     one the following formats:
                        {ipv4_address}
                        [{ipv6_address}]
                        {ipv6_address}

        Returns:
            A valid IpPort

        Raises:
          ValueError
        """
        try:
            # Remove [] that might be surrounding an IPv6 address
            ip = ip.replace("[", "").replace("]", "")
            return IpPort(ipaddress.ip_address(ip), None)
        except ValueError as e:
            raise e


@dataclass(frozen=True)
class TargetSshAddress(IpPort):
    """Dataclass that holds target's ssh address information.

    Args:
        ip: Target's SSH IP Address
        port: Target's SSH port
    """


@dataclass(frozen=True)
class Sl4fServerAddress(IpPort):
    """Dataclass that holds sl4f server address information.

    Args:
        ip: IP Address of SL4F server
        port: Port where SL4F server is listening for SL4F requests
    """


@dataclass(frozen=True)
class DeviceInfo:
    """Dataclass that holds Fuchsia device information.

    Args:
        name: Device name returned by `ffx target list`.
        serial_socket: Device serial socket path.
        ip_port: IP Address and port of the device.
    """

    name: str
    ip_port: IpPort | None
    serial_socket: str | None

    def __str__(self) -> str:
        return (
            f"name={self.name}, "
            f"ip_port={self.ip_port}, "
            f"serial_socket={self.serial_socket}, "
        )


@dataclass(frozen=True)
class FidlEndpoint:
    """Dataclass that holds FIDL end point information.

    Args:
        moniker: moniker pointing to the FIDL end point
        protocol: protocol name of the FIDL end point
    """

    moniker: str
    protocol: str
