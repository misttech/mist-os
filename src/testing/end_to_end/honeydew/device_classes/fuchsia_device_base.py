#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Default implementation of FuchsiaDevice abstract base class."""

import logging
import subprocess
import time
from functools import lru_cache
from typing import Any, Dict, Iterable, Optional, Type

from honeydew import custom_types, errors
from honeydew.interfaces.affordances import component
from honeydew.interfaces.device_classes import (
    component_capable_device, fuchsia_device)
from honeydew.utils import ffx_cli, host_utils, http_utils

_LOGGER = logging.getLogger(__name__)

_CMDS = {
    "ECHO": "echo",
    "START_SL4F": "start_sl4f",
}

_SL4F_PORT = 80
_SL4F_METHODS = {
    "GetDeviceName": "device_facade.GetDeviceName",
    "GetDeviceInfo": "hwinfo_facade.HwinfoGetDeviceInfo",
    "GetProductInfo": "hwinfo_facade.HwinfoGetProductInfo",
    "GetVersion": "device_facade.GetVersion",
}

_TIMEOUTS = {
    "SL4F_RESPONSE": 30,
    "SSH_COMMAND_ARG": 3,
    "SSH_COMMAND_RESPONSE": 60,
}

_SSH_OPTIONS_TUPLE = (
    "-oPasswordAuthentication=no", "-oStrictHostKeyChecking=no",
    f"-oConnectTimeout={_TIMEOUTS['SSH_COMMAND_ARG']}")
_SSH_OPTIONS = " ".join(_SSH_OPTIONS_TUPLE)
_SSH_COMMAND = \
    "ssh {ssh_options} -i {ssh_pkey} {ssh_user}@{ip_address} {command}"


# pylint: disable=attribute-defined-outside-init, too-many-instance-attributes
class FuchsiaDeviceBase(fuchsia_device.FuchsiaDevice,
                        component_capable_device.ComponentCapableDevice):
    """Default implementation of Fuchsia device abstract base class.

    This class contains methods that are supported by every device running
    Fuchsia irrespective of the device type.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_pkey: Absolute path to the SSH private key file needed to SSH
            into fuchsia device. Either pass the value here or set value in
            'SSH_PRIVATE_KEY_FILE' environmental variable.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

        device_ip_address: Device IP (V4|V6) address. If not provided, attempts
            to resolve automatically.

    Raises:
        errors.FuchsiaDeviceError: Failed to instantiate.
    """

    def __init__(
            self,
            device_name: str,
            ssh_pkey: Optional[str] = fuchsia_device.DEFAULT_SSH_PKEY,
            ssh_user: str = fuchsia_device.DEFAULT_SSH_USER,
            device_ip_address: Optional[str] = None) -> None:
        super().__init__(device_name, ssh_pkey, ssh_user, device_ip_address)
        self._start_sl4f_server()

    # List all the static properties in alphabetical order
    @property
    @lru_cache
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return ffx_cli.get_target_type(self.name)

    # Not decorating with @lru_cache as self._product_info is already decorated.
    @property
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["manufacturer"]

    # Not decorating with @lru_cache as self._product_info is already decorated.
    @property
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["model"]

    # Not decorating with @lru_cache as self._product_info is already decorated.
    @property
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["name"]

    @property
    @lru_cache
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_device_info_resp = self._send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceInfo"])
        return get_device_info_resp["result"]["serial_number"]

    # List all the dynamic properties in alphabetical order
    @property
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_version_resp = self._send_sl4f_command(
            method=_SL4F_METHODS["GetVersion"])
        return get_version_resp["result"]

    # List all the affordances in alphabetical order
    @property
    def component(self) -> component.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """
        raise NotImplementedError

    # List all the public methods in alphabetical order
    def close(self) -> None:
        """Clean up method."""
        raise NotImplementedError

    def log_message_to_device(self, message: str, level: custom_types.LEVEL):
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """
        raise NotImplementedError

    def reboot(self) -> None:
        """Soft reboot the device."""
        raise NotImplementedError

    def snapshot(
            self, directory: str, snapshot_file: Optional[str] = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file need
                to be saved.

            snapshot_file: Name of the file to be used to save snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.
        """
        raise NotImplementedError

    # List all private methods in alphabetical order
    def _check_sl4f_connection(self):
        """Checks SL4F connection by sending a SL4F request to device.

        Raises:
            errors.FuchsiaDeviceError: If SL4F connection is not successful.
        """
        get_device_name_resp = self._send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceName"])
        device_name = get_device_name_resp["result"]

        if device_name != self.name:
            raise errors.FuchsiaDeviceError(
                f"Failed to start SL4F server on '{self.name}'.")

    @property
    @lru_cache
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_product_info_resp = self._send_sl4f_command(
            method=_SL4F_METHODS["GetProductInfo"])
        return get_product_info_resp["result"]

    def _run_ssh_command_on_host(
            self,
            command: str,
            timeout: float = _TIMEOUTS["SSH_COMMAND_RESPONSE"]) -> str:
        """Runs SSH command on the host and returns the output.

        Args:
            command: SSH command to run.
            timeout: How long in sec to wait for SSH command to complete.

        Returns:
            SSH command output.

        Raises:
            CalledProcessError: On failure.
        """
        ssh_command = _SSH_COMMAND.format(
            ssh_options=_SSH_OPTIONS,
            ssh_pkey=self._ssh_pkey,
            ssh_user=self._ssh_user,
            ip_address=self._ip_address,
            command=command)

        _LOGGER.debug("Running the SSH command: '%s'...", ssh_command)
        output = subprocess.check_output(
            ssh_command.split(), timeout=timeout).decode()
        return output

    # pylint: disable=too-many-arguments
    def _send_sl4f_command(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        attempts: int = 3,
        interval: int = 3,
        exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
    ) -> Dict[str, Any]:
        """Sends SL4F command from host to Fuchsia device and returns the
        response.

        Args:
            method: SL4F method.
            params: Any optional params needed for method param.
            attempts: number of attempts to try in case of a failure.
            interval: wait time in sec before each retry in case of a failure.
            exceptions_to_skip: Any non fatal exceptions for which retry will
                not be attempted.

        Returns:
            SL4F command response returned by the Fuchsia device.
                Note: If SL4F command raises any exception specified in
                exceptions_to_skip then a empty dict will be returned.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        if params is None:
            params = {}

        if exceptions_to_skip is None:
            exceptions_to_skip = []

        # pylint: disable=protected-access
        if host_utils._get_ip_version(self._ip_address) == 6:
            url = f"http://[{self._ip_address}]:{_SL4F_PORT}"
        else:
            url = f"http://{self._ip_address}:{_SL4F_PORT}"

        # id is required by the SL4F server to parse test_data but is not
        # currently used.
        data = {"jsonrpc": "2.0", "id": "", "method": method, "params": params}

        exception_msg = f"SL4F method '{method}' failed on '{self.name}'."
        for attempt in range(1, attempts + 1):
            # if this is not first attempt wait for sometime before next retry.
            if attempt > 1:
                time.sleep(interval)
            try:
                http_response = http_utils.send_http_request(
                    url,
                    data,
                    attempts=attempts,
                    interval=interval,
                    exceptions_to_skip=exceptions_to_skip)
                error = http_response.get('error')

                if error is None:
                    return http_response

                if attempt < attempts:  # pylint: disable=no-else-continue
                    _LOGGER.warning(
                        "SL4F method '%s' failed with error: '%s' on "
                        "iteration %s/%s", method, error, attempt, attempts)
                    continue
                else:
                    exception_msg += f" Error: '{error}'."
                    break

            except Exception as err:
                raise errors.FuchsiaDeviceError(exception_msg) from err
        raise errors.FuchsiaDeviceError(exception_msg)

    def _start_sl4f_server(self) -> None:
        """Starts the SL4F server on fuchsia device.

        Raises:
            errors.FuchsiaDeviceError: Failed to start the SL4F server.
        """
        _LOGGER.info("Starting SL4F server on %s...", self.name)
        self._run_ssh_command_on_host(command=_CMDS['START_SL4F'])

        # verify the device is responsive to SL4F requests
        self._check_sl4f_connection()
