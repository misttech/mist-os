#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via SSH."""

import logging
import subprocess
import time
from typing import Any, Dict, List, Optional

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import ffx as ffx_transport

_DEFAULTS: Dict[str, Any] = {
    "USERNAME": "fuchsia",
}

_CMDS: Dict[str, str] = {
    "ECHO": "echo",
}

_TIMEOUTS: Dict[str, float] = {
    "COMMAND_ARG": 3,
    "COMMAND_RESPONSE": 60,
    "CONNECTION": 60,
}

_OPTIONS_LIST: List[str] = [
    "-oPasswordAuthentication=no",
    "-oStrictHostKeyChecking=no",
    f"-oConnectTimeout={_TIMEOUTS['COMMAND_ARG']}",
]
_OPTIONS: str = " ".join(_OPTIONS_LIST)
_SSH_COMMAND: str = "ssh {options} -i {private_key} -p {port} " \
                    "{username}@{ip_address} {command}"

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SSH:
    """Provides methods for Host-(Fuchsia)Target interactions via SSH.

    Args:
        name: Fuchsia device name.

        private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        username: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".
    """

    def __init__(
            self,
            device_name: str,
            private_key: str,
            username: Optional[str] = None) -> None:
        self._name: str = device_name
        self._private_key: str = private_key
        self._username: str = username or _DEFAULTS["USERNAME"]

    def check_connection(
            self, timeout: float = _TIMEOUTS["CONNECTION"]) -> None:
        """Checks the SSH connection from host to Fuchsia device.

        Args:
            timeout: How long in sec to wait for SSH connection.

        Raises:
            errors.SSHCommandError: If fails to establish SSH connection.
        """
        start_time: float = time.time()
        end_time: float = start_time + timeout

        _LOGGER.debug("Waiting for %s to allow ssh connection...", self._name)
        while time.time() < end_time:
            try:
                self.run(command=_CMDS["ECHO"])
                break
            except Exception:  # pylint: disable=broad-except
                time.sleep(1)
        else:
            raise errors.SSHCommandError(
                f"Failed to connect to '{self._name}' via SSH.")
        _LOGGER.debug("%s is available via ssh.", self._name)

    def run(
            self,
            command: str,
            timeout: float = _TIMEOUTS["COMMAND_RESPONSE"]) -> str:
        """Run command on Fuchsia device from host via SSH and return output.

        Args:
            command: Command to run on the Fuchsia device.
            timeout: How long in sec to wait for SSH command to complete.

        Returns:
            Command output.

        Raises:
            errors.SSHCommandError: On failure.
            errors.FfxCommandError: If failed to get the target SSH address.
        """
        ffx = ffx_transport.FFX(target=self._name)
        target_ssh_address: custom_types.TargetSshAddress = \
            ffx.get_target_ssh_address()

        ssh_command: str = _SSH_COMMAND.format(
            options=_OPTIONS,
            private_key=self._private_key,
            port=target_ssh_address.port,
            username=self._username,
            ip_address=target_ssh_address.ip,
            command=command)
        try:
            _LOGGER.debug("Running the SSH command: '%s'...", ssh_command)
            output: str = subprocess.check_output(
                ssh_command.split(), timeout=timeout).decode()
            _LOGGER.debug(
                "Output returned by SSH command '%s' is: '%s'", ssh_command,
                output)
            return output
        except subprocess.CalledProcessError as err:
            if err.stdout:
                _LOGGER.debug(
                    "stdout returned by the command is: %s", err.stdout)
            if err.stderr:
                _LOGGER.debug(
                    "stderr returned by the command is: %s", err.stdout)

            raise errors.SSHCommandError(err) from err
        except Exception as err:  # pylint: disable=broad-except
            raise errors.SSHCommandError(err) from err
