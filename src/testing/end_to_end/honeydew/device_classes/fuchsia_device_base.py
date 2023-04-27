#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Default implementation of FuchsiaDevice abstract base class."""

import base64
import ipaddress
import logging
import os
import subprocess
import sys
import time
from datetime import datetime
from http.client import RemoteDisconnected
from typing import Any, Dict, Iterable, List, Optional, Type

from honeydew import custom_types, errors
from honeydew.affordances import bluetooth_default, component_default
from honeydew.interfaces.affordances import bluetooth as bluetooth_interface
from honeydew.interfaces.affordances import component as component_interface
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.interfaces.device_classes import (
    bluetooth_capable_device, component_capable_device, fuchsia_device)
from honeydew.interfaces.transports import sl4f
from honeydew.utils import ffx_cli, http_utils, properties

_CMDS: Dict[str, str] = {
    "ECHO": "echo",
    "START_SL4F": "start_sl4f",
}

_SL4F_PORT: Dict[str, int] = {
    "LOCAL": 80,
    # To support common Fuchsia.git in-tree remote workflow where users are
    # running fx serve-remote
    "REMOTE": 9080,
}

_SL4F_METHODS: Dict[str, str] = {
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

_TIMEOUTS: Dict[str, float] = {
    "BOOT_UP_COMPLETE": 60,
    "OFFLINE": 60,
    "ONLINE": 60,
    "SNAPSHOT": 60,
    "SSH_COMMAND_ARG": 3,
    "SSH_COMMAND_RESPONSE": 60,
}

_SSH_OPTIONS_LIST: List[str] = [
    "-oPasswordAuthentication=no",
    "-oStrictHostKeyChecking=no",
    f"-oConnectTimeout={_TIMEOUTS['SSH_COMMAND_ARG']}",
]
_SSH_OPTIONS: str = " ".join(_SSH_OPTIONS_LIST)
_SSH_COMMAND = "ssh {ssh_options} -i {ssh_private_key} -p {ssh_port} " \
               "{ssh_user}@{ip_address} {command}"

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDeviceBase(fuchsia_device.FuchsiaDevice, sl4f.SL4F,
                        component_capable_device.ComponentCapableDevice,
                        bluetooth_capable_device.BluetoothCapableDevice):
    """Default implementation of Fuchsia device abstract base class.

    This class contains methods that are supported by every device running
    Fuchsia irrespective of the device type.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Raises:
        errors.FuchsiaDeviceError: Failed to instantiate.
    """

    def __init__(
            self,
            device_name: str,
            ssh_private_key: str,
            ssh_user: str = fuchsia_device.DEFAULTS["SSH_USER"]) -> None:
        super().__init__(device_name, ssh_private_key, ssh_user)
        self._sl4f_port: int = self._get_device_sl4f_port(self._ip_address)
        self.start_sl4f_server()

    # List all the persistent properties in alphabetical order
    @properties.PersistentProperty
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return ffx_cli.get_target_type(self.name)

    @properties.PersistentProperty
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["manufacturer"]

    @properties.PersistentProperty
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["model"]

    @properties.PersistentProperty
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["name"]

    @properties.PersistentProperty
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_device_info_resp: Dict[str, Any] = self.send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceInfo"])
        return get_device_info_resp["result"]["serial_number"]

    # List all the dynamic properties in alphabetical order
    @properties.DynamicProperty
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_version_resp: Dict[str, Any] = self.send_sl4f_command(
            method=_SL4F_METHODS["GetVersion"])
        return get_version_resp["result"]

    # List all the affordances in alphabetical order
    # TODO(fxbug.dev/123944): Remove this after fxbug.dev/123944 is fixed
    @properties.Affordance
    def bluetooth(self) -> bluetooth_interface.Bluetooth:
        """Returns a bluetooth affordance object.

        Returns:
            bluetooth.Bluetooth object
        """
        return bluetooth_default.BluetoothDefault(
            device_name=self.name, sl4f=self)

    @properties.Affordance
    def component(self) -> component_interface.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """
        return component_default.ComponentDefault(
            device_name=self.name, sl4f=self)

    # List all the public methods in alphabetical order
    def check_sl4f_connection(self) -> None:
        """Checks SL4F connection by sending a SL4F request to device.

        Raises:
            errors.FuchsiaDeviceError: If SL4F connection is not successful.
        """
        get_device_name_resp: Dict[str, Any] = self.send_sl4f_command(
            method=_SL4F_METHODS["GetDeviceName"])
        device_name: str = get_device_name_resp["result"]

        if device_name != self.name:
            raise errors.FuchsiaDeviceError(
                f"Failed to start SL4F server on '{self.name}'.")

    def close(self) -> None:
        """Clean up method."""
        return

    def log_message_to_device(
            self, message: str, level: custom_types.LEVEL) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
        message = f"[HoneyDew] - [Host Time: {timestamp}] - {message}"
        self.send_sl4f_command(
            method=_SL4F_METHODS[f"Log{level.name.capitalize()}"],
            params={"message": message})

    def power_cycle(
            self,
            power_switch: power_switch_interface.PowerSwitch,
            outlet: Optional[int] = None) -> None:
        """Power cycle (power off, wait for delay, power on) the device.

        Args:
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.
        """
        _LOGGER.info("Power cycling %s...", self.name)
        self.log_message_to_device(
            message=f"Powering cycling {self.name}...",
            level=custom_types.LEVEL.INFO)

        _LOGGER.info("Powering off %s...", self.name)
        power_switch.power_off(outlet)
        self._wait_for_offline()

        _LOGGER.info("Powering on %s...", self.name)
        power_switch.power_on(outlet)
        self._wait_for_bootup_complete()

        self.log_message_to_device(
            message=f"Successfully power cycled {self.name}...",
            level=custom_types.LEVEL.INFO)

    def reboot(self) -> None:
        """Soft reboot the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        _LOGGER.info("Rebooting %s...", self.name)
        self.log_message_to_device(
            message=f"Rebooting {self.name}...", level=custom_types.LEVEL.INFO)
        self.send_sl4f_command(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected])
        self._wait_for_offline()
        self._wait_for_bootup_complete()
        self.log_message_to_device(
            message=f"Successfully rebooted {self.name}...",
            level=custom_types.LEVEL.INFO)

    def send_sl4f_command(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        timeout: float = sl4f.TIMEOUTS["SL4F_RESPONSE"],
        attempts: int = sl4f.DEFAULTS["ATTEMPTS"],
        interval: int = sl4f.DEFAULTS["INTERVAL"],
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
        if ipaddress.ip_address(self._normalize_ip_addr(
                self._ip_address)).version == 6:
            url: str = f"http://[{self._ip_address}]:{self._sl4f_port}"
        else:
            url = f"http://{self._ip_address}:{self._sl4f_port}"

        # id is required by the SL4F server to parse test_data but is not
        # currently used.
        data: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": "",
            "method": method,
            "params": params
        }

        exception_msg: str = f"SL4F method '{method}' failed on '{self.name}'."
        for attempt in range(1, attempts + 1):
            # if this is not first attempt wait for sometime before next retry.
            if attempt > 1:
                time.sleep(interval)
            try:
                http_response: Dict[str, Any] = http_utils.send_http_request(
                    url,
                    data,
                    timeout=timeout,
                    attempts=attempts,
                    interval=interval,
                    exceptions_to_skip=exceptions_to_skip)

                error: str | None = http_response.get("error")
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
            timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
            snapshot_file = f"Snapshot_{self.name}_{timestamp}.zip"
        snapshot_file_path: str = os.path.join(directory, snapshot_file)

        _LOGGER.info("Collecting snapshot on %s...", self.name)

        snapshot_resp: Dict[str, Any] = self.send_sl4f_command(
            method=_SL4F_METHODS["Snapshot"], timeout=_TIMEOUTS["SNAPSHOT"])
        snapshot_base64_encoded_str: str = snapshot_resp["result"]["zip"]
        snapshot_base64_decoded_bytes: bytes = base64.b64decode(
            snapshot_base64_encoded_str)

        with open(snapshot_file_path, "wb") as snapshot_binary_zip:
            snapshot_binary_zip.write(snapshot_base64_decoded_bytes)

        _LOGGER.info("Snapshot file has been saved @ '%s'", snapshot_file_path)
        return snapshot_file_path

    def start_sl4f_server(self) -> None:
        """Starts the SL4F server on fuchsia device.

        Raises:
            errors.FuchsiaDeviceError: Failed to start the SL4F server.
        """
        _LOGGER.info("Starting SL4F server on %s...", self.name)
        self._run_ssh_command_on_host(command=_CMDS["START_SL4F"])

        # verify the device is responsive to SL4F requests
        self.check_sl4f_connection()

    # List all private methods in alphabetical order
    def _check_ssh_connection_to_device(self, timeout: float) -> None:
        """Checks the SSH connection from host to Fuchsia device.

        Args:
            timeout: How long in sec to wait for SSH connection.

        Raises:
            errors.FuchsiaDeviceError: If fails to establish SSH connection.
        """
        start_time: float = time.time()
        end_time: float = start_time + timeout

        _LOGGER.info("Waiting for %s to allow ssh connection...", self.name)
        while time.time() < end_time:
            try:
                self._run_ssh_command_on_host(command=_CMDS["ECHO"])
                break
            except Exception:  # pylint: disable=broad-except
                time.sleep(1)
        else:
            raise errors.FuchsiaDeviceError(
                f"Failed to connect to '{self.name}' via SSH.")
        _LOGGER.info("%s is available via ssh.", self.name)

    def _get_device_sl4f_port(self, device_ip_address: str) -> int:
        """Returns the port on which SL4F server running on device is listening.

        Args:
            device_ip_address: Device IP address.

        Returns:
            SL4F Port
        """
        # Device addr is localhost, assume that means that ports were forwarded
        # from a remote workstation/laptop with a device attached.
        if ipaddress.ip_address(
                self._normalize_ip_addr(device_ip_address)).is_loopback:
            return _SL4F_PORT["REMOTE"]
        return _SL4F_PORT["LOCAL"]

    def _get_device_ssh_address(
            self, timeout: float) -> custom_types.TargetSshAddress:
        """Returns the device SSH information (ip address and port).

        Args:
            timeout: How long in sec to retry for ip address.

        Returns:
            (Target SSH IP Address, Target SSH Port)

        Raises:
            errors.FuchsiaDeviceError: If fails to gets the ip address.
        """
        start_time: float = time.time()
        end_time: float = start_time + timeout

        while time.time() < end_time:
            try:
                return ffx_cli.get_target_ssh_address(
                    self.name, timeout=timeout)
            except Exception:  # pylint: disable=broad-except
                time.sleep(1)
        raise errors.FuchsiaDeviceError(
            f"Failed to get the ip address of '{self.name}'.")

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

    @properties.PersistentProperty
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_product_info_resp: Dict[str, Any] = self.send_sl4f_command(
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
            errors.SSHCommandError: On failure.
        """
        ssh_command: str = _SSH_COMMAND.format(
            ssh_options=_SSH_OPTIONS,
            ssh_private_key=self._ssh_private_key,
            ssh_port=self._ssh_port,
            ssh_user=self._ssh_user,
            ip_address=self._ip_address,
            command=command)
        try:
            _LOGGER.debug("Running the SSH command: '%s'...", ssh_command)
            output: str = subprocess.check_output(
                ssh_command.split(), timeout=timeout).decode()
            _LOGGER.debug(
                "Output returned by SSH command '%s' is: '%s'", ssh_command,
                output)
            return output
        except Exception as err:  # pylint: disable=broad-except
            raise errors.SSHCommandError(err) from err

    def _wait_for_bootup_complete(
            self, timeout: float = _TIMEOUTS["BOOT_UP_COMPLETE"]) -> None:
        """Wait for Fuchsia device to complete the boot.

        Args:
            timeout: How long in sec to wait for bootup.

        Raises:
            errors.FuchsiaDeviceError: If bootup operation(s) fail.
        """
        end_time: float = time.time() + timeout

        # wait until device is responsive to FFX
        self._wait_for_online(timeout)

        # IP address may change on reboot. So get the ip address once again
        remaining_timeout: float = max(0, end_time - time.time())

        device_ssh_address = self._get_device_ssh_address(remaining_timeout)
        self._ip_address = device_ssh_address.ip
        self._ssh_port = device_ssh_address.port
        self._sl4f_port = self._get_device_sl4f_port(self._ip_address)

        # wait until device to allow ssh connection
        remaining_timeout = max(0, end_time - time.time())
        self._check_ssh_connection_to_device(remaining_timeout)

        # Restart SL4F server on the device
        self.start_sl4f_server()

        # If applicable, initialize bluetooth stack
        if "qemu" not in self.device_type:
            self.bluetooth.sys_init()

    def _wait_for_offline(self, timeout: float = _TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not offline.
        """
        _LOGGER.info("Waiting for %s to go offline...", self.name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if not ffx_cli.check_ffx_connection(self.name):
                _LOGGER.info("%s is offline.", self.name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to go offline in {timeout}sec.")

    def _wait_for_online(self, timeout: float = _TIMEOUTS["ONLINE"]) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not online.
        """
        _LOGGER.info("Waiting for %s to go online...", self.name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if ffx_cli.check_ffx_connection(self.name):
                _LOGGER.info("%s is online.", self.name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to go online in {timeout}sec.")
