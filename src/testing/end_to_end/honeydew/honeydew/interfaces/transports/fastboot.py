#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via Fastboot."""

import abc
from collections.abc import Iterable

from honeydew.utils import properties

TIMEOUTS: dict[str, float] = {
    "FASTBOOT_CLI": 30,
    "FASTBOOT_MODE": 45,
    "FUCHSIA_MODE": 45,
    "TCP_ADDRESS": 30,
}


class Fastboot(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via Fastboot."""

    @properties.PersistentProperty
    @abc.abstractmethod
    def node_id(self) -> str:
        """Fastboot node id.

        Returns:
            Fastboot node value.
        """

    @abc.abstractmethod
    def boot_to_fastboot_mode(self) -> None:
        """Boot the device to fastboot mode from fuchsia mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: Failed to boot the device to fastboot
                mode.
        """

    @abc.abstractmethod
    def boot_to_fuchsia_mode(self) -> None:
        """Boot the device to fuchsia mode from fastboot mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: Failed to boot the device to fuchsia
                mode.
        """

    @abc.abstractmethod
    def is_in_fastboot_mode(self) -> bool:
        """Checks if device is in fastboot mode or not.

        Returns:
            True if in fastboot mode, False otherwise.

        Raises:
            errors.FastbootCommandError: If failed to check the fastboot mode.
        """

    @abc.abstractmethod
    def run(
        self,
        cmd: list[str],
        timeout: float = TIMEOUTS["FASTBOOT_CLI"],
        exceptions_to_skip: Iterable[type[Exception]] | None = None,
    ) -> list[str]:
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

    @abc.abstractmethod
    def wait_for_fastboot_mode(
        self, timeout: float = TIMEOUTS["FASTBOOT_MODE"]
    ) -> None:
        """Wait for Fuchsia device to go to fastboot mode.

        Args:
            timeout: How long in sec to wait for device to go fastboot mode.

        Raises:
            errors.FuchsiaDeviceError: If device is not in fastboot mode.
        """

    @abc.abstractmethod
    def wait_for_fuchsia_mode(
        self, timeout: float = TIMEOUTS["FUCHSIA_MODE"]
    ) -> None:
        """Wait for Fuchsia device to go to fuchsia mode.

        Args:
            timeout: How long in sec to wait for device to go fuchsia mode.

        Raises:
            errors.FuchsiaDeviceError: If device is not in fuchsia mode.
        """
