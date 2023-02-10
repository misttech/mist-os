#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utility module for local host commands."""

import ipaddress
import logging
import subprocess
import sys
from typing import Optional

_LOGGER = logging.getLogger(__name__)

_DEFAULT_PING_PARAMS = {
    "TIMEOUT": 5,
    "PACKET_COUNT": 3,
}
_PING_CMD = {4: "ping", 6: "ping6"}


# List all the public methods in alphabetical order
def is_pingable(
        ip_address: str,
        timeout: float = _DEFAULT_PING_PARAMS["TIMEOUT"],
        packet_count: int = _DEFAULT_PING_PARAMS["PACKET_COUNT"],
        deadline: Optional[float] = None) -> bool:
    """Checks if the ip_address responds to network pings.

    Args:
        ip_address (str): IPv4|IPv6 address.
        timeout (float): Timeout in seconds to wait for a ping response.
        packet_count (int): How many packets will be sent before the deadline.
        deadline (float): Timeout in seconds before ping exits regardless of how
            many packets have been sent or received.

    Returns:
        bool: True if the IP is (valid and) pingable, otherwise False.
    """
    if not _validate_ip_address(ip_address):
        return False

    flags_map: dict[str, Optional[float]] = {
        "-c": packet_count,
        "-W": timeout,
        "-w": deadline
    }

    cmd_list = [_get_ping_cmd(ip_address)]
    for flag, flag_value in flags_map.items():
        if flag_value is not None:
            cmd_list.append(flag)
            cmd_list.append(str(flag_value))
    cmd_list.append(ip_address)

    try:
        _LOGGER.debug("Running the command '%s'...", ' '.join(cmd_list))
        subprocess.check_output(cmd_list, stderr=subprocess.STDOUT)
        _LOGGER.debug("%s is pingable", ip_address)
        return True
    except subprocess.CalledProcessError:
        _LOGGER.debug("%s is not pingable", ip_address)
        return False


# List all private methods in alphabetical order
def _get_ip_version(ip_address: str) -> int:
    """Returns the version (v4 or v6) of the ip address.

    Args:
        ip_address (str): IPv4|IPv6 address.

    Returns:
        int: IP Version (4 or 6).
    """
    return ipaddress.ip_address(_normalize_ip_addr(ip_address)).version


def _get_ping_cmd(ip_address: str) -> str:
    """Returns the ping command to use based on the ip address specified.

    Args:
        ip_address (str): IPv4|IPv6 address.

    Returns:
        str: Ping command to use
    """
    return _PING_CMD[_get_ip_version(ip_address)]


def _normalize_ip_addr(ip_address: str) -> str:
    """Workaround IPv6 scope IDs for Python 3.8 or older

    IPv6 scope identifiers are not supported in Python 3.8. Added a workaround
    to remove the scope identifier from IPv6 address if python version is 3.8 or
    lower.

    Ex: In fe80::1f91:2f5c:5e9b:7ff3%qemu IPv6 address, "%qemu" is the scope
    identifier.

    Args:
        ip_address (str): IPv4|IPv6 address.

    Returns:
        str: ip address with or without scope identifier based on python
            version.
    """
    if sys.version_info < (3, 9):
        ip_address = ip_address.split('%')[0]
    return ip_address


def _validate_ip_address(ip_address: str) -> bool:
    """Checks if the ip address specified is of a valid ip address format.

    Args:
        ip_address (str): IPv4|IPv6 address.

    Returns:
        bool: True if it is a valid ip address format, else False.
    """
    try:
        ipaddress.ip_address(_normalize_ip_addr(ip_address))
    except Exception:  # pylint: disable=broad-except
        return False
    return True
