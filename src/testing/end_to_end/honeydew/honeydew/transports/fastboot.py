#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via Fastboot."""
import logging
import subprocess
from typing import Any, Dict, Iterable, List, Optional, Type

from honeydew import errors
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.utils import common
from honeydew.utils import properties

_FASTBOOT_CMDS: Dict[str, List[str]] = {
    "BOOT_TO_FUCHSIA_MODE": ["reboot"],
}

_FFX_CMDS: Dict[str, List[str]] = {
    "BOOT_TO_FASTBOOT_MODE": ["target", "ssh", "dm", "reboot-bootloader"],
}

_TIMEOUTS: Dict[str, float] = {
    "FASTBOOT_CLI": 30,
    "FASTBOOT_MODE": 45,
    "FUCHSIA_MODE": 45,
    "TCP_ADDRESS": 30,
}

_NO_SERIAL = "<unknown>"

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Fastboot:
    """Provides methods for Host-(Fuchsia)Target interactions via Fastboot.

    Args:
        device_name: Fuchsia device name.

    Raises:
        errors.FuchsiaDeviceError: Failed to get the fastboot node id
    """

    def __init__(
            self,
            device_name: str,
            reboot_affordance: affordances_capable.RebootCapableDevice,
            fastboot_node_id: Optional[str] = None) -> None:
        self._device_name: str = device_name
        self._reboot_affordance: affordances_capable.RebootCapableDevice = \
            reboot_affordance
        self._ffx: ffx_transport.FFX = ffx_transport.FFX(self._device_name)
        self._get_fastboot_node(fastboot_node_id)

    # List all the public properties in alphabetical order
    @properties.PersistentProperty
    def node_id(self) -> str:
        """Fastboot node id.

        Returns:
            Fastboot node value.
        """
        return self._fastboot_node_id

    # List all the public methods in alphabetical order
    def boot_to_fastboot_mode(self) -> None:
        """Boot the device to fastboot mode from fuchsia mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: Failed to boot the device to fastboot
                mode.
        """
        if not self.is_in_fuchsia_mode():
            raise errors.FuchsiaStateError(
                f"'{self._device_name}' is not in fuchsia mode to perform " \
                f"this operation."
            )

        try:
            self._ffx.run(
                cmd=_FFX_CMDS["BOOT_TO_FASTBOOT_MODE"],
                exceptions_to_skip=[subprocess.CalledProcessError])
            self._wait_for_fastboot_mode()
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FastbootCommandError(
                f"Failed to reboot {self._device_name} to fastboot mode from "\
                f"fuchsia mode"
            ) from err

    def boot_to_fuchsia_mode(self) -> None:
        """Boot the device to fuchsia mode from fastboot mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: Failed to boot the device to fuchsia
                mode.
        """
        if not self.is_in_fastboot_mode():
            raise errors.FuchsiaStateError(
                f"'{self._device_name}' is not in fastboot mode to perform " \
                f"this operation."
            )

        try:
            self.run(cmd=_FASTBOOT_CMDS["BOOT_TO_FUCHSIA_MODE"])
            self._wait_for_fuchsia_mode()
            self._reboot_affordance.wait_for_online()
            self._reboot_affordance.on_device_boot()

        except Exception as err:  # pylint: disable=broad-except
            raise errors.FastbootCommandError(
                f"Failed to reboot {self._device_name} to fuchsia mode from "\
                f"fastboot mode"
            ) from err

    def is_in_fastboot_mode(self) -> bool:
        """Checks if device is in fastboot mode or not.

        Returns:
            True if in fastboot mode, False otherwise.
        """
        try:
            target_info: Dict[str, Any] = self._get_target_info()
        except errors.FfxCommandError as err:
            _LOGGER.warning(err)
            return False

        return (
            target_info["nodename"], target_info["rcs_state"],
            target_info["target_state"]) == (
                self._device_name, "N", "Fastboot")

    def is_in_fuchsia_mode(self) -> bool:
        """Checks if device is in fuchsia mode or not.

        Returns:
            True if in fuchsia mode, False otherwise.
        """
        return self._ffx.is_target_connected()

    def run(
        self,
        cmd: List[str],
        timeout: float = _TIMEOUTS["FASTBOOT_CLI"],
        exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
    ) -> List[str]:
        """Executes and returns the output of `fastboot -s {node} {cmd}`.

        Args:
            cmd: Fastboot command to run.
            timeout: Timeout to wait for the fastboot command to return.
            exceptions_to_skip: Any non fatal exceptions to be ignored.

        Returns:
            Output of `fastboot -s {node} {cmd}`.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            subprocess.TimeoutExpired: Timeout running a fastboot command.
            errors.FastbootCommandError: In case of failure.
        """
        if not self.is_in_fastboot_mode():
            raise errors.FuchsiaStateError(
                f"'{self._device_name}' is not in fastboot mode to perform " \
                f"this operation."
            )

        exceptions_to_skip = tuple(exceptions_to_skip or [])

        fastboot_cmd: List[str] = ["fastboot", "-s", self.node_id] + cmd
        try:
            _LOGGER.debug("Executing command `%s`", " ".join(fastboot_cmd))
            output: str = subprocess.check_output(
                fastboot_cmd, stderr=subprocess.STDOUT,
                timeout=timeout).decode().strip()

            _LOGGER.debug("`%s` returned: %s", " ".join(fastboot_cmd), output)

            # Remove the last entry which will contain command execution time
            # 'Finished. Total time: 0.001s'
            return output.split("\n")[:-1]
        except Exception as err:  # pylint: disable=broad-except
            if isinstance(err, exceptions_to_skip):
                return []

            if isinstance(err, subprocess.TimeoutExpired):
                _LOGGER.debug(err, exc_info=True)
                raise

            raise errors.FastbootCommandError(
                f"`{fastboot_cmd}` command failed") from err

    # List all the private methods in alphabetical order
    def _get_fastboot_node(
            self, fastboot_node_id: Optional[str] = None) -> None:
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
            target: Dict[str, Any] = self._get_target_info()

            # USB based fastboot connection
            if target.get("serial", _NO_SERIAL) != _NO_SERIAL:
                self._fastboot_node_id = target["serial"]
                return
            else:  # TCP based fastboot connection
                self.boot_to_fastboot_mode()

                self._wait_for_valid_tcp_address()

                target = self._get_target_info()
                target_address: str = target["addresses"][0]
                tcp_address: str = f"tcp:{target_address}"

                self._fastboot_node_id = tcp_address

                # before calling `boot_to_fuchsia_mode()`,
                # self._fastboot_node_id need to be populated
                self.boot_to_fuchsia_mode()
                return
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaDeviceError(
                f"Failed to get the fastboot node id of '{self._device_name}'"
            ) from err

    def _get_target_info(self) -> Dict[str, Any]:
        """Return the target information

        Returns:
            target information

        Raises:
            errors.FfxCommandError: If target is not connected to host.
        """
        for target in self._ffx.get_target_list():
            if target["nodename"] == self._device_name:
                return target
        raise errors.FfxCommandError(
            f"'{self._device_name}' is not connected to host")

    def _is_a_single_ip_address(self) -> bool:
        """Returns True if "address" field of `ffx target show` has one ip
        address, false otherwise.

        Returns:
            True if "address" field of `ffx target show` has one ip address,
            False otherwise.
        """
        target: Dict[str, Any] = self._get_target_info()
        return len(target["addresses"]) == 1

    def _wait_for_fastboot_mode(
            self, timeout: float = _TIMEOUTS["FASTBOOT_MODE"]) -> None:
        """Wait for Fuchsia device to go to fastboot mode.

        Args:
            timeout: How long in sec to wait for device to go fastboot mode.

        Raises:
            errors.FuchsiaDeviceError: If device is not in fastboot mode.
        """
        _LOGGER.info("Waiting for %s to go fastboot mode...", self._device_name)

        try:
            common.wait_for_state(
                state_fn=self.is_in_fastboot_mode,
                expected_state=True,
                timeout=timeout)
            _LOGGER.info("%s is in fastboot mode...", self._device_name)
        except errors.HoneyDewTimeoutError as err:
            raise errors.FuchsiaDeviceError(
                f"'{self._device_name}' failed to go into fastboot mode in "\
                f"{timeout}sec."
            ) from err

    def _wait_for_fuchsia_mode(
            self, timeout: float = _TIMEOUTS["FUCHSIA_MODE"]) -> None:
        """Wait for Fuchsia device to go to fuchsia mode.

        Args:
            timeout: How long in sec to wait for device to go fuchsia mode.

        Raises:
            errors.FuchsiaDeviceError: If device is not in fuchsia mode.
        """
        _LOGGER.info("Waiting for %s to go fuchsia mode...", self._device_name)

        try:
            common.wait_for_state(
                state_fn=self.is_in_fuchsia_mode,
                expected_state=True,
                timeout=timeout)
            _LOGGER.info("%s is in fuchsia mode...", self._device_name)
        except errors.HoneyDewTimeoutError as err:
            raise errors.FuchsiaDeviceError(
                f"'{self._device_name}' failed to go into fuchsia mode in "\
                f"{timeout}sec.") from err

    def _wait_for_valid_tcp_address(
            self, timeout: float = _TIMEOUTS["TCP_ADDRESS"]) -> None:
        """Wait for Fuchsia device to have a valid TCP address.

        Args:
            timeout: How long in sec to wait for a valid TCP address.

        Raises:
            errors.FuchsiaDeviceError: If failed to get valid TCP address.
        """
        _LOGGER.info(
            "Waiting for a valid TCP address assigned to %s in fastboot "\
            "mode...", self._device_name)

        try:
            common.wait_for_state(
                state_fn=self._is_a_single_ip_address,
                expected_state=True,
                timeout=timeout)
        except errors.HoneyDewTimeoutError as err:
            raise errors.FuchsiaDeviceError(
                f"Unable to get the TCP address of '{self._device_name}' " \
                f"when in fastboot mode "
            ) from err
