#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom data types."""

from __future__ import annotations

from dataclasses import dataclass
import enum
import ipaddress
from typing import NamedTuple, Optional, Union

import fuchsia_controller_py as fuchsia_controller


class LEVEL(enum.Enum):
    """Logging level that need to specified to log a message onto device"""
    INFO = enum.auto()
    WARNING = enum.auto()
    ERROR = enum.auto()


class IpPort(NamedTuple):
    """Tuple that contains an IP Address and Port

    Args:
        ip: Ip Address
        port: Port Number
    """
    ip: Union[ipaddress.IPv4Address, ipaddress.IPv6Address]
    port: int

    def __str__(self) -> str:
        host: str = f"{self.ip}"
        if isinstance(self.ip, ipaddress.IPv6Address):
            host = f"[{host}]"
        return f"{host}:{self.port}"

    @staticmethod
    def parse(target_name: str) -> IpPort:
        """Checks if the given string is an IPAddress Port or not

      Args:
        target_name: the name of the target to test. This is of format
                     {ipv4_address}:{port}, or [{ipv6_address}]:{port},
                     or {ipv6_address}:{port}


      Returns:
        A valid IpPort if the str is one. Otherwise None

      Raises:
        ValueError
      """
        try:
            # If we have something of form
            #     192.168.1.1:8888 ==> ["192.168.1.1", "8888"]
            # If we have something of form
            #     [::1]:8888 ==> ["[::1]", "8888"]
            arr: list[str] = target_name.rsplit(":", 1)
            if len(arr) != 2:
                raise ValueError(
                    f"Value: {target_name} was not a valid IpPort (needs " \
                    f"IP Address and Port)"
                )
            addr_part: str = arr[0]
            port_part: str = arr[1]
            # Remove [] that might be surrounding an IPv6 address
            addr_part = addr_part.replace("[", "").replace("]", "")
            port = int(port_part)
            if port < 1:
                raise ValueError(
                    f"For IpPort: {target_name}, port number: {port} was " \
                    f"not a positive integer)"
                )
            return IpPort(ipaddress.ip_address(addr_part), port)
        except ValueError as e:
            raise e


class TargetSshAddress(NamedTuple):
    """Tuple that holds target's ssh address information.

    Args:
        ip: Target's SSH IP Address
        port: Target's SSH port
    """
    ip: str
    port: int


class Sl4fServerAddress(NamedTuple):
    """Tuple that holds sl4f server address information.

    Args:
        ip: IP Address of SL4F server
        port: Port where SL4F server is listening for SL4F requests
    """
    ip: str
    port: int


@dataclass
class FFXConfig:
    """Dataclass that holds FFX config information.

    Args:
        isolate_dir: FFX isolation directory
        logs_dir: FFX logs directory
    """
    isolate_dir: Optional[fuchsia_controller.IsolateDir] = None
    logs_dir: Optional[str] = None
