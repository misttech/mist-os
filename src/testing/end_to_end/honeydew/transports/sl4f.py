#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via SL4F."""

import ipaddress
import logging
import sys
import time
from typing import Any, Dict, Iterable, List, Optional, Type

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import ffx as ffx_transport
from honeydew.utils import http_utils
from honeydew.utils import properties

_TIMEOUTS: Dict[str, float] = {
    "RESPONSE": 30,
}

_DEFAULTS: Dict[str, int] = {
    "ATTEMPTS": 3,
    "INTERVAL": 3,
}

_FFX_CMDS: Dict[str, List[str]] = {
    "START_SL4F": ["target", "ssh", "start_sl4f"],
}

_SL4F_PORT: Dict[str, int] = {
    "LOCAL": 80,
    # To support common Fuchsia.git in-tree remote workflow where users are
    # running fx serve-remote
    "REMOTE": 9080,
}

_SL4F_METHODS: Dict[str, str] = {
    "GetDeviceName": "device_facade.GetDeviceName",
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SL4F:
    """Provides methods for Host-(Fuchsia)Target interactions via SL4F.

    Args:
        device_name: Fuchsia device name.

    Raises:
        errors.Sl4fError: Failed to instantiate.
    """

    def __init__(self, device_name: str) -> None:
        self._name: str = device_name
        self._ffx = ffx_transport.FFX(target=self._name)

        self.start_server()

    # List all the properties in alphabetical order
    @properties.DynamicProperty
    def url(self) -> str:
        """URL of the SL4F server.

        Returns:
            URL of the SL4F server.

        Raises:
            errors.Sl4fError: On failure.
        """
        sl4f_server_address: custom_types.Sl4fServerAddress = \
            self._get_sl4f_server_address()

        if self._get_ip_version(sl4f_server_address.ip) == 6:
            return \
                f"http://[{sl4f_server_address.ip}]:{sl4f_server_address.port}"
        else:
            return \
                f"http://{sl4f_server_address.ip}:{sl4f_server_address.port}"

    # List all the public methods in alphabetical order
    def check_connection(self) -> None:
        """Check SL4F connection between host and SL4F server running on device.

        Raises:
            errors.Sl4fError: If SL4F connection is not successful.
        """
        get_device_name_resp: Dict[str, Any] = self.run(
            method=_SL4F_METHODS["GetDeviceName"])
        device_name: str = get_device_name_resp["result"]

        if device_name != self._name:
            raise errors.Sl4fError(
                f"Failed to start SL4F server on '{self._name}'.")

    def run(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        timeout: float = _TIMEOUTS["RESPONSE"],
        attempts: int = _DEFAULTS["ATTEMPTS"],
        interval: int = _DEFAULTS["INTERVAL"],
        exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
    ) -> Dict[str, Any]:
        """Run the SL4F method on Fuchsia device and return the response.

        Args:
            method: SL4F method.
            params: Any optional params needed for method param.
            timeout: Timeout in seconds to wait for SL4F request to complete.
            attempts: number of attempts to try in case of a failure.
            interval: wait time in sec before each retry in case of a failure.
            exceptions_to_skip: Any non fatal exceptions for which retry will
                not be attempted and no error will be raised.

        Returns:
            SL4F command response returned by the Fuchsia device.
                Note: If SL4F command raises any exception specified in
                exceptions_to_skip then a empty dict will be returned.

        Raises:
            errors.Sl4fError: On failure.
        """
        if not params:
            params = {}

        if not exceptions_to_skip:
            exceptions_to_skip = []

        # id is required by the SL4F server to parse test_data but is not
        # currently used.
        data: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": "",
            "method": method,
            "params": params
        }

        exception_msg: str = f"SL4F method '{method}' failed on '{self._name}'."
        for attempt in range(1, attempts + 1):
            # if this is not first attempt wait for sometime before next retry.
            if attempt > 1:
                time.sleep(interval)
            try:
                http_response: Dict[str, Any] = http_utils.send_http_request(
                    self.url,
                    data,
                    timeout=timeout,
                    attempts=attempts,
                    interval=interval,
                    exceptions_to_skip=exceptions_to_skip)

                error: Optional[str] = http_response.get("error")
                if not error:
                    return http_response

                if attempt < attempts:
                    _LOGGER.warning(
                        "SL4F method '%s' failed with error: '%s' on "
                        "iteration %s/%s", method, error, attempt, attempts)
                    continue
                else:
                    exception_msg = f"{exception_msg} Error: '{error}'."
                    break

            except Exception as err:
                raise errors.Sl4fError(exception_msg) from err
        raise errors.Sl4fError(exception_msg)

    def start_server(self) -> None:
        """Starts the SL4F server on fuchsia device.

        Raises:
            errors.Sl4fError: Failed to start the SL4F server.
        """
        _LOGGER.info("Starting SL4F server on %s...", self._name)

        try:
            self._ffx.run(cmd=_FFX_CMDS["START_SL4F"])
        except Exception as err:  # pylint: disable=broad-except
            raise errors.Sl4fError(err) from err

        # verify the device is responsive to SL4F requests
        self.check_connection()

    # List all private methods in alphabetical order
    def _get_sl4f_server_address(self) -> custom_types.Sl4fServerAddress:
        """Returns the SL4F server ip address and port information.

        Returns:
            (SL4F Server IP Address, SL4F Server Port)

        Raises:
            errors.Sl4fError: In case of failure.
            errors.FfxCommandError: If failed to get the SL4F server address.
        """
        sl4f_server_ip: str = self._ffx.get_target_ssh_address().ip

        # Device addr is localhost, assume that means that ports were forwarded
        # from a remote workstation/laptop with a device attached.
        sl4f_port: int = _SL4F_PORT["LOCAL"]
        if ipaddress.ip_address(
                self._normalize_ip_addr(sl4f_server_ip)).is_loopback:
            sl4f_port = _SL4F_PORT["REMOTE"]

        return custom_types.Sl4fServerAddress(ip=sl4f_server_ip, port=sl4f_port)

    def _get_ip_version(self, ip_address: str) -> int:
        """Returns the ip version (4 | 6).

        Args:
            ip_address: IPv4|IPv6 address.

        Returns:
            ip version (4 | 6).
        """
        ip_address = self._normalize_ip_addr(ip_address)
        return ipaddress.ip_address(ip_address).version

    def _normalize_ip_addr(self, ip_address: str) -> str:
        """Workaround IPv6 scope IDs for Python 3.8 or older.

        IPv6 scope identifiers are not supported in Python 3.8. Added a
        workaround to remove the scope identifier from IPv6 address if python
        version is 3.8 or lower.

        Ex: In fe80::1f91:2f5c:5e9b:7ff3%qemu IPv6 address, "%qemu" is the scope
        identifier.

        Args:
            ip_address: IPv4|IPv6 address.

        Returns:
            ip address with or without scope identifier based on python version.
        """
        if sys.version_info < (3, 9):
            ip_address = ip_address.split("%")[0]
        return ip_address
