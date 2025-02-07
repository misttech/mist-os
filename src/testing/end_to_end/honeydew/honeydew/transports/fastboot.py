# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via Fastboot."""

import atexit
import logging
import os
import shutil
import stat
import tempfile
import time
from importlib import resources
from typing import Any

from honeydew import errors
from honeydew.auxiliary_devices.power_switch import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import fastboot as fastboot_interface
from honeydew.interfaces.transports import ffx as ffx_interface
from honeydew.interfaces.transports import serial as serial_interface
from honeydew.utils import common, host_shell, properties

_FASTBOOT_PATH_ENV_VAR = "HONEYDEW_FASTBOOT_OVERRIDE"

_FASTBOOT_CMDS: dict[str, list[str]] = {
    "BOOT_TO_FUCHSIA_MODE": ["reboot"],
    "IS_IN_FASTBOOT_MODE": ["getvar", "serialno"],
}

_FFX_CMDS: dict[str, list[str]] = {
    "BOOT_TO_FASTBOOT_MODE": ["target", "reboot", "--bootloader"],
}


_NO_SERIAL = "<unknown>"

_LOGGER: logging.Logger = logging.getLogger(__name__)


# List all the private methods
def _get_fastboot_binary() -> str:
    """Returns the path to the `fastboot` binary.

    Prefers resolving from environment variable if provided; otherwise, extract
    from Python resource, set permissions to executable, and store on disk.

    Returns:
        Absolute path to `fastboot` binary.
    """
    bin_path: str | None = os.getenv(_FASTBOOT_PATH_ENV_VAR)
    if bin_path is not None:
        return bin_path
    try:
        # Import resources via the data package name specified in this library's
        # build definition.
        # If Honeydew is run outside of the build system, this package will not
        # be present so we wrap the import in a try-except block.
        # pylint: disable-next=import-outside-toplevel
        from honeydew import data  # type: ignore[attr-defined,unused-ignore]

        bin_fd = tempfile.NamedTemporaryFile(suffix="fastboot", delete=False)
        bin_path = bin_fd.name
        with resources.as_file(resources.files(data).joinpath("fastboot")) as f:
            f.chmod(f.stat().st_mode | stat.S_IEXEC)
            shutil.copy2(f, bin_path)
        atexit.register(lambda: os.unlink(bin_path))
    except ImportError as e:
        raise errors.HoneydewDataResourceError(
            "Failed to import data resource. If running outside of build system,"
            f" supply the `{_FASTBOOT_PATH_ENV_VAR}` environment variable.",
        ) from e
    return bin_path


class Fastboot(fastboot_interface.Fastboot):
    """Provides methods for Host-(Fuchsia)Target interactions via Fastboot.

    Args:
        device_name: Fuchsia device name.
        reboot_affordance: Object to RebootCapableDevice implementation.
        ffx_transport: Object to FFX transport interface implementation.
        fastboot_node_id: Fastboot Node ID.

    Raises:
        errors.FuchsiaDeviceError: Failed to get the fastboot node id
    """

    def __init__(
        self,
        device_name: str,
        reboot_affordance: affordances_capable.RebootCapableDevice,
        ffx_transport: ffx_interface.FFX,
        fastboot_node_id: str | None = None,
    ) -> None:
        self._device_name: str = device_name
        self._reboot_affordance: affordances_capable.RebootCapableDevice = (
            reboot_affordance
        )
        self._ffx_transport: ffx_interface.FFX = ffx_transport
        self._fastboot_binary: str = _get_fastboot_binary()
        self._get_fastboot_node(fastboot_node_id)

    # List all the public properties
    @properties.PersistentProperty
    def node_id(self) -> str:
        """Fastboot node id.

        Returns:
            Fastboot node value.
        """
        return self._fastboot_node_id

    # List all the public methods
    def boot_to_fastboot_mode(
        self,
        use_serial: bool = False,
        serial_transport: serial_interface.Serial | None = None,
        power_switch: power_switch_interface.PowerSwitch | None = None,
        outlet: int | None = None,
    ) -> None:
        """Boot the device to fastboot mode from fuchsia mode.

        Args:
            use_serial: Use serial port on the device to boot into Fastboot mode.
                If set to True, user need to also pass serial_transport, power_switch and outlet
                args so that device can be power cycled.
            serial_transport: Implementation of Serial interface.
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FuchsiaDeviceError: Failed to boot the device to fastboot
                mode.
        """
        # Note: Rebooting the device into fastboot mode using serial is mostly used when device is
        # in bad state. So Do not check if device is in Fuchsia mode and directly perform the
        # operation.
        if use_serial is False:
            try:
                self.wait_for_fuchsia_mode()
            except errors.FuchsiaDeviceError as err:
                raise errors.FuchsiaStateError(
                    f"'{self._device_name}' is not in fuchsia mode to perform "
                    f"this operation."
                ) from err

        try:
            if use_serial:
                self._boot_to_fastboot_mode_using_serial(
                    serial_transport,
                    power_switch,
                    outlet,
                )
            else:
                self._boot_to_fastboot_mode_using_ffx()
        except errors.HoneydewError as err:
            raise errors.FuchsiaDeviceError(
                f"Failed to boot {self._device_name} into fastboot mode"
            ) from err

        self.wait_for_fastboot_mode()

    def boot_to_fuchsia_mode(self) -> None:
        """Boot the device to fuchsia mode from fastboot mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FuchsiaDeviceError: Failed to boot the device to fuchsia
                mode.
        """
        if not self.is_in_fastboot_mode():
            raise errors.FuchsiaStateError(
                f"'{self._device_name}' is not in fastboot mode to perform "
                f"this operation."
            )

        try:
            self.run(cmd=_FASTBOOT_CMDS["BOOT_TO_FUCHSIA_MODE"])
            self.wait_for_fuchsia_mode()
            self._reboot_affordance.wait_for_online()
            self._reboot_affordance.on_device_boot()
        except errors.HoneydewError as err:
            raise errors.FuchsiaDeviceError(
                f"Failed to reboot {self._device_name} to fuchsia mode from "
                f"fastboot mode"
            ) from err

    def is_in_fastboot_mode(self) -> bool:
        """Checks if device is in fastboot mode or not.

        Returns:
            True if in fastboot mode, False otherwise.

        Raises:
            errors.FastbootCommandError: Failed to check if device is in fastboot mode or not.
        """
        _LOGGER.debug(
            "Checking if '%s' is in fastboot mode or not", self._device_name
        )
        try:
            output: str = (
                host_shell.run(
                    cmd=[
                        self._fastboot_binary,
                        "devices",
                    ],
                )
                or ""
            )
        except errors.HostCmdError as err:
            raise errors.FastbootCommandError(
                f"Failed to check if {self._device_name} is in fastboot mode or not"
            ) from err

        fastboot_devices: list[str] = output.strip().split("\n")
        for fastboot_device in fastboot_devices:
            if self._fastboot_node_id.lower() in fastboot_device.lower():
                _LOGGER.info("'%s' is in fastboot mode", self._device_name)
                return True
        _LOGGER.info("'%s' is not in fastboot mode", self._device_name)
        return False

    def run(
        self,
        cmd: list[str],
    ) -> list[str]:
        """Executes and returns the output of `fastboot -s {node} {cmd}`.

        Args:
            cmd: Fastboot command to run.

        Returns:
            Output of `fastboot -s {node} {cmd}`.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: In case of failure.
        """
        if not self.is_in_fastboot_mode():
            raise errors.FuchsiaStateError(
                f"'{self._device_name}' is not in fastboot mode to perform "
                f"this operation."
            )

        fastboot_cmd: list[str] = [
            self._fastboot_binary,
            "-s",
            self.node_id,
        ] + cmd

        try:
            output: str = (
                host_shell.run(
                    cmd=fastboot_cmd,
                    # fastboot cmd output is coming in stderr instead of stdout
                    capture_error_in_output=True,
                )
                or ""
            )
            # Remove the last entry which will contain command execution time
            # 'Finished. Total time: 0.001s'
            return output.strip().split("\n")[:-1]

        except errors.HostCmdError as err:
            raise errors.FastbootCommandError(err) from err

    def wait_for_fastboot_mode(self) -> None:
        """Wait for Fuchsia device to go to fastboot mode."""
        _LOGGER.info("Waiting for %s to go fastboot mode...", self._device_name)
        common.wait_for_state(
            state_fn=self.is_in_fastboot_mode,
            expected_state=True,
        )

    def wait_for_fuchsia_mode(self) -> None:
        """Wait for Fuchsia device to go to fuchsia mode."""
        _LOGGER.info("Waiting for %s to go fuchsia mode...", self._device_name)
        self._ffx_transport.wait_for_rcs_connection()
        _LOGGER.info("%s is in fuchsia mode...", self._device_name)

    # List all the private methods
    def _boot_to_fastboot_mode_using_ffx(self) -> None:
        """Boot the device to fastboot mode using FFX."""
        _LOGGER.info(
            "Lacewing is booting the following device to fastboot mode: %s",
            self._device_name,
        )
        try:
            self._ffx_transport.run(cmd=_FFX_CMDS["BOOT_TO_FASTBOOT_MODE"])
        except errors.FfxCommandError:
            # Command is expected to fail as device reboots immediately
            pass

    # TODO(b/359261703): Once issue is resolved, remove `| None` from type hint for `serial_transport` and `power_switch`
    def _boot_to_fastboot_mode_using_serial(
        self,
        serial_transport: serial_interface.Serial | None,
        power_switch: power_switch_interface.PowerSwitch | None,
        outlet: int | None = None,
    ) -> None:
        """Boot the device to fastboot mode using serial.

        Args:
            serial_transport: Implementation of Serial interface.
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.

        Raises:
            ValueError: Invalid args sent.
        """
        if power_switch is None or serial_transport is None:
            raise ValueError(
                f"'power_switch' and 'serial_transport' args need to be provided to reboot "
                f"'{self._device_name}' into Fastboot using Serial"
            )

        # Note: Rebooting the device into fastboot mode using serial is mostly used when device is
        # in bad state. So Do not check if device is in Fuchsia mode and directly perform the operation

        _LOGGER.info("Powering off %s...", self._device_name)
        power_switch.power_off(outlet)

        _LOGGER.info("Powering on %s...", self._device_name)
        power_switch.power_on(outlet)

        _LOGGER.info(
            "Lacewing is booting the following device to fastboot mode: %s",
            self._device_name,
        )

        start_time: float = time.time()
        end_time: float = start_time + 10

        while time.time() < end_time:
            serial_transport.send(cmd="f")
            # Do not send continuously, it will fill buffers very quickly
            time.sleep(0.2)

    def _get_fastboot_node(self, fastboot_node_id: str | None = None) -> None:
        """Gets the fastboot node id and stores it in `self._fastboot_node_id`.

        Runs `ffx target list` and look for corresponding device information.
        use serial number as fastboot node id if available, otherwise fall back
        to the TCP address.

        Raises:
            errors.FuchsiaDeviceError: Failed to get the fastboot node id
        """
        if fastboot_node_id:
            self._fastboot_node_id: str = fastboot_node_id
            return

        try:
            target: dict[
                str, Any
            ] = self._ffx_transport.get_target_info_from_target_list()

            # USB based fastboot connection
            if target.get("serial", _NO_SERIAL) != _NO_SERIAL:
                self._fastboot_node_id = target["serial"]
                return
            else:  # TCP based fastboot connection
                self.boot_to_fastboot_mode()

                self._wait_for_valid_tcp_address()

                target = self._ffx_transport.get_target_info_from_target_list()
                target_address: str = target["addresses"][0]
                tcp_address: str = f"tcp:{target_address}"

                self._fastboot_node_id = tcp_address

                # before calling `boot_to_fuchsia_mode()`,
                # self._fastboot_node_id need to be populated
                self.boot_to_fuchsia_mode()
                return
        except errors.HoneydewError as err:
            raise errors.FuchsiaDeviceError(
                f"Failed to get the fastboot node id of '{self._device_name}'"
            ) from err

    def _is_a_single_ip_address(self) -> bool:
        """Returns True if "address" field of `ffx target show` has one ip
        address, false otherwise.

        Returns:
            True if "address" field of `ffx target show` has one ip address,
            False otherwise.
        """
        target: dict[
            str, Any
        ] = self._ffx_transport.get_target_info_from_target_list()
        return len(target["addresses"]) == 1

    def _wait_for_valid_tcp_address(self) -> None:
        """Wait for Fuchsia device to have a valid TCP address."""
        _LOGGER.debug(
            "Waiting for a valid TCP address assigned to %s in fastboot "
            "mode...",
            self._device_name,
        )
        common.wait_for_state(
            state_fn=self._is_a_single_ip_address,
            expected_state=True,
        )
        _LOGGER.debug(
            "Valid TCP address has been assigned to %s in the fastboot mode.",
            self._device_name,
        )
