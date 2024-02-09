#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via SSH."""

import abc
import subprocess
from typing import Any

from honeydew.typing import custom_types

TIMEOUTS: dict[str, float] = {
    "COMMAND_RESPONSE": 60,
    "CONNECTION": 60,
    "FFX_CLI": 10,
}


class SSH(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via SSH."""

    @abc.abstractmethod
    def check_connection(self, timeout: float = TIMEOUTS["CONNECTION"]) -> None:
        """Checks the SSH connection from host to Fuchsia device.

        Args:
            timeout: How long in sec to wait for SSH connection.

        Raises:
            errors.SshConnectionError
        """

    @abc.abstractmethod
    def get_target_address(
        self, timeout: float | None = TIMEOUTS["FFX_CLI"]
    ) -> custom_types.TargetSshAddress:
        """Gets the address used on SSH.

        Args:
            timeout: How long in sec to wait to get the device's SSH address.

        Returns:
            The IP address and port used for SSH.
        """

    @abc.abstractmethod
    def run(
        self,
        command: str,
        timeout: float | None = TIMEOUTS["COMMAND_RESPONSE"],
        get_ssh_addr_timeout: float | None = TIMEOUTS["FFX_CLI"],
    ) -> str:
        """Run command on Fuchsia device from host via SSH and return output.

        Args:
            command: Command to run on the Fuchsia device.
            timeout: How long in sec to wait for SSH command to complete.
            get_ssh_addr_timeout: If `ip_port` was not provided during the
                initialization, `FFX` transport will be used to get the device
                SSH address.


        Returns:
            Command output.

        Raises:
            errors.SSHCommandError: On failure.
            errors.FfxCommandError: If failed to get the target SSH address.
        """

    @abc.abstractmethod
    def popen(
        self,
        command: str,
        get_ssh_addr_timeout: float | None = TIMEOUTS["FFX_CLI"],
    ) -> subprocess.Popen[Any]:
        """Run command on Fuchsia device from host via SSH and return the
        underlying subprocess.

        It is up to callers to detect and handle potential errors, and make sure
        to close this process eventually (e.g. with `popen.terminate` method).


        Args:
            command: Command to run on the Fuchsia device.
            get_ssh_addr_timeout: If `ip_port` was not provided during the
                initialization, the `FFX` transport will be used to get the
                device SSH address.

        Returns:
            The underlying subprocess.

        Raises:
            errors.SSHCommandError: On failure.
            errors.FfxCommandError: If failed to get the target SSH address.
        """
