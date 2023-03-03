#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Default implementation of FuchsiaDevice abstract base class."""

import base64
import logging
import os
import subprocess
import time
from datetime import datetime
from functools import lru_cache
from http.client import RemoteDisconnected
from typing import Any, Dict, Iterable, Optional, Type

from honeydew import custom_types, errors
from honeydew.interfaces.affordances import component
from honeydew.interfaces.device_classes import (
    component_capable_device, fuchsia_device)
from honeydew.interfaces.transports import sl4f
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
    "LogError": "logging_facade.LogErr",
    "LogInfo": "logging_facade.LogInfo",
    "LogWarning": "logging_facade.LogWarn",
    "Reboot": "hardware_power_statecontrol_facade.SuspendReboot",
    "Snapshot": "feedback_data_provider_facade.GetSnapshot",
}

_TIMEOUTS = {
    "BOOT_UP_COMPLETE": 60,
    "OFFLINE": 60,
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
class FuchsiaDeviceBase(fuchsia_device.FuchsiaDevice, sl4f.SL4F,
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
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._device_type

    @property
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["manufacturer"]

    @property
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["model"]

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
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._serial_number

    # List all the dynamic properties in alphabetical order
    @property
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_version_resp = self.send_sl4f_command(
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
        return

    def log_message_to_device(self, message: str, level: custom_types.LEVEL):
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self.send_sl4f_command(
            method=_SL4F_METHODS[f"Log{level.name.capitalize()}"],
            params={"message": message})

    def reboot(self) -> None:
        """Soft reboot the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        _LOGGER.info("Rebooting %s...", self.name)
        self.send_sl4f_command(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected])
        self._wait_for_offline()
        self._wait_for_bootup_complete()

    # pylint: disable=too-many-arguments
    def send_sl4f_command(
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
                not be attempted and no error will be raised.

        Returns:
            SL4F command response returned by the Fuchsia device.
                Note: If SL4F command raises any exception specified in
                exceptions_to_skip then a empty dict will be returned.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        if not params:
            params = {}

        if not exceptions_to_skip:
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
                if not error:
                    return http_response

                if attempt < attempts:  # pylint: disable=no-else-continue
                    _LOGGER.warning(
                        "SL4F method '%s' failed with error: '%s' on "
                        "iteration %s/%s", method, error, attempt, attempts)
                    continue
                else:
                    exception_msg = f"{exception_msg} Error: '{error}'."
                    break

            except Exception as err:
                raise errors.FuchsiaDeviceError(exception_msg) from err
        raise errors.FuchsiaDeviceError(exception_msg)

    def snapshot(
            self, directory: str, snapshot_file: Optional[str] = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file will be
                saved. If this directory does not exist, this method will create
                it.

            snapshot_file: Name of the output snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        directory = os.path.abspath(directory)
        try:
            os.makedirs(directory)
        except FileExistsError:
            pass

        if not snapshot_file:
            timestamp = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
            snapshot_file = f"Snapshot_{self.name}_{timestamp}.zip"
        snapshot_file_path = os.path.join(directory, snapshot_file)

        snapshot_resp = self.send_sl4f_command(method=_SL4F_METHODS["Snapshot"])
        snapshot_base64_encoded_str = snapshot_resp["result"]["zip"]
        snapshot_base64_decoded_bytes = base64.b64decode(
            snapshot_base64_encoded_str)

        with open(snapshot_file_path, "wb") as snapshot_binary_zip:
            snapshot_binary_zip.write(snapshot_base64_decoded_bytes)

        _LOGGER.info("Snapshot file has been saved @ '%s'", snapshot_file_path)
        return snapshot_file_path

    # List all private methods in alphabetical order
    def _check_sl4f_connection(self):
        """Checks SL4F connection by sending a SL4F request to device.

        Raises:
            errors.FuchsiaDeviceError: If SL4F connection is not successful.
        """
        get_device_name_resp = self.send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceName"])
        device_name = get_device_name_resp["result"]

        if device_name != self.name:
            raise errors.FuchsiaDeviceError(
                f"Failed to start SL4F server on '{self.name}'.")

    def _check_ssh_connection_to_device(self, timeout: float):
        """Checks the SSH connection from host to Fuchsia device.

        Args:
            timeout: How long in sec to wait for SSH connection.

        Raises:
            errors.FuchsiaDeviceError: If fails to establish SSH connection.
        """
        start_time = time.time()
        end_time = start_time + timeout

        _LOGGER.info("Waiting for %s to allow ssh connection...", self.name)
        while time.time() < end_time:
            try:
                self._run_ssh_command_on_host(command=_CMDS['ECHO'])
                break
            except Exception:  # pylint: disable=broad-except
                time.sleep(1)
        else:
            raise errors.FuchsiaDeviceError(
                f"Failed to connect to '{self.name}' via SSH.")
        _LOGGER.info("%s is available via ssh.", self.name)

    @property
    @lru_cache
    def _device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FuchsiaDeviceError: On failure.

        Note:
            Created this method instead of directly calling this method's
            implementation in `device_type` to address type hinting error.
        """
        return ffx_cli.get_target_type(self.name)

    def _get_device_ip_address(self, timeout: float):
        """Returns the device IP(V4|V6) address used for SSHing from host.

        Args:
            timeout: How long in sec to retry for ip address.

        Raises:
            errors.FuchsiaDeviceError: If fails to gets the ip address.
        """
        start_time = time.time()
        end_time = start_time + timeout

        while time.time() < end_time:
            try:
                return ffx_cli.get_target_address(self.name, timeout=timeout)
            except Exception:  # pylint: disable=broad-except
                time.sleep(1)
        raise errors.FuchsiaDeviceError(
            f"Failed to get the ip address of '{self.name}'.")

    def _ping_device(self, timeout: float):
        """Pings Fuchsia device from the host.

        Args:
            timeout: How long in sec to wait for ping to succeed.

        Raises:
            errors.FuchsiaDeviceError: If ping is not successful.
        """
        start_time = time.time()
        end_time = start_time + timeout

        # wait until device is responsive to pings
        _LOGGER.info("Waiting for %s to become pingable...", self.name)
        while time.time() < end_time:
            if host_utils.is_pingable(self._ip_address):
                break
            time.sleep(1)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to become pingable in {timeout}sec.")
        _LOGGER.info("%s now pingable.", self.name)

    @property
    @lru_cache
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_product_info_resp = self.send_sl4f_command(
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
            subprocess.CalledProcessError: On failure.
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

    @property
    @lru_cache
    def _serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.

        Note:
            Created this method instead of directly calling this method's
            implementation in `device_type` to address type hinting error.
        """
        get_device_info_resp = self.send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceInfo"])
        return get_device_info_resp["result"]["serial_number"]

    def _start_sl4f_server(self) -> None:
        """Starts the SL4F server on fuchsia device.

        Raises:
            errors.FuchsiaDeviceError: Failed to start the SL4F server.
        """
        _LOGGER.info("Starting SL4F server on %s...", self.name)
        self._run_ssh_command_on_host(command=_CMDS['START_SL4F'])

        # verify the device is responsive to SL4F requests
        self._check_sl4f_connection()

    def _wait_for_bootup_complete(
            self, timeout: float = _TIMEOUTS["BOOT_UP_COMPLETE"]) -> None:
        """Wait for Fuchsia device to complete the boot.

        Args:
            timeout: How long in sec to wait for bootup.

        Raises:
            errors.FuchsiaDeviceError: If bootup operation(s) fail.
        """
        end_time = time.time() + timeout

        # IP address may change on reboot. So get the ip address once again
        self._ip_address = self._get_device_ip_address(timeout)

        # wait until device is responsive to pings
        remaining_timeout = max(0, end_time - time.time())
        self._ping_device(remaining_timeout)

        # wait until device to allow ssh connection
        remaining_timeout = max(0, end_time - time.time())
        self._check_ssh_connection_to_device(remaining_timeout)

        # Restart SL4F server on the device
        self._start_sl4f_server()

    def _wait_for_offline(self, timeout: float = _TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not offline.
        """
        # wait until device is not responsive to pings
        _LOGGER.info("Waiting for %s to go offline...", self.name)
        start_time = time.time()
        end_time = start_time + timeout
        count = 0
        while time.time() < end_time:
            if not host_utils.is_pingable(self._ip_address):
                count += 1  # Ensure device is really offline not just a blip
            else:
                count = 0
            if count == 2:
                _LOGGER.info("%s is offline.", self.name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to go offline in {timeout}sec.")
