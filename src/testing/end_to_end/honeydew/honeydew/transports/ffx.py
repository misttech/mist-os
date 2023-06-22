#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via FFX."""

import json
import logging
from shutil import rmtree
import subprocess
import tempfile
from typing import Any, Dict, Iterable, List, Optional, Type

from honeydew import custom_types
from honeydew import errors

_FFX_CMDS: Dict[str, List[str]] = {
    "TARGET_ADD": ["target", "add"],
    "TARGET_SHOW": ["target", "show", "--json"],
    "TARGET_SSH_ADDRESS": ["target", "get-ssh-address"],
    "TARGET_LIST": ["--machine", "json", "target", "list"],
}

_TIMEOUTS: Dict[str, float] = {
    "FFX_CLI": 10,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)

_ISOLATE_DIR: Optional[str] = None
_LOGS_DIR: Optional[str] = None


def setup(logs_dir: str) -> None:
    """Set up method.

    It does the following:
    * Creates a new isolation dir needed to run FFX in isolation mode.
      This results in spawning a new FFX daemon.
    * Initializes a directory for storing FFX logs (ffx.log and ffx.daemon.log).

    Args:
        logs_dir: Directory for storing FFX logs (ffx.log and ffx.daemon.log).
                  If not passed, FFX logs will not be stored.

    Raises:
        errors.FfxCommandError: If this method is called more than once.

    Note:
    * This method should be called only once to ensure daemon logs are going to
      single location.
    * If this method is not called then FFX logs will not be saved and will use
      the system level FFX daemon (instead of spawning new one using isolation).
    * Ensure to call close() in the end if you have called this setup() method.
      Otherwise, FFX daemon start by this method will not be killed.
    """
    global _ISOLATE_DIR
    global _LOGS_DIR

    if _ISOLATE_DIR or _LOGS_DIR:
        raise errors.FfxCommandError("setup has already been called once.")

    if _ISOLATE_DIR is None:
        _ISOLATE_DIR = tempfile.mkdtemp()

    if _LOGS_DIR is None:
        _LOGS_DIR = logs_dir

    _LOGGER.debug("ffx isolation dir is: '%s'", _ISOLATE_DIR)
    _LOGGER.debug("ffx logs dir is '%s'", _LOGS_DIR)


def close() -> None:
    """Clean up method.

    It does the following:
    * Deletes the isolation directory created during `setup()`
    """
    global _ISOLATE_DIR
    global _LOGS_DIR

    try:
        _LOGGER.debug("Deleting ffx isolation dir: '%s'", _ISOLATE_DIR)
        rmtree(str(_ISOLATE_DIR))
    except FileNotFoundError:
        pass

    _ISOLATE_DIR = None
    _LOGS_DIR = None


class FFX:
    """Provides methods for Host-(Fuchsia)Target interactions via FFX.

    Args:
        target: Fuchsia device name.
    """

    def __init__(self, target: str) -> None:
        self._target: str = target

    @staticmethod
    def add_target(
            target_ip_port: custom_types.IpPort,
            timeout: float = _TIMEOUTS["FFX_CLI"]):
        """Adds a target to the ffx collection

        Args:
            target: Target IpPort.

        Returns:
            True for success, False otherwise

        Raises:
            subprocess.TimeoutExpired: In case of timeout
            errors.FfxCommandError: In case of failure.
        """
        cmd: List[str] = FFX._generate_ffx_cmd(
            target=None, cmd=_FFX_CMDS["TARGET_ADD"])
        cmd.append(str(target_ip_port))
        try:
            _LOGGER.debug("Executing command `%s`", " ".join(cmd))
            output: str = subprocess.check_output(
                cmd, stderr=subprocess.STDOUT, timeout=timeout).decode()
            _LOGGER.debug("`%s` returned: %s", " ".join(cmd), output)
        except subprocess.TimeoutExpired as err:
            _LOGGER.debug(err, exc_info=True)
            raise
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def check_connection(self, timeout: float = _TIMEOUTS["FFX_CLI"]) -> None:
        """Checks the FFX connection from host to Fuchsia device.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Raises:
            errors.FFXCommandError: If device is not connected to the host
                according to FFX.
        """
        if self.is_target_connected(timeout=timeout):
            return
        else:
            raise errors.FfxCommandError(f"'{self._target}' is not connected.")

    def get_target_information(
            self,
            timeout: float = _TIMEOUTS["FFX_CLI"]) -> List[Dict[str, Any]]:
        """Executed and returns the output of `ffx -t {target} target show`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx -t {target} target show`.

        Raises:
            subprocess.TimeoutExpired: In case of timeout
            errors.FfxCommandError: In case of failure.
        """
        cmd: List[str] = _FFX_CMDS["TARGET_SHOW"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)

            ffx_target_show_info: List[Dict[str, Any]] = json.loads(output)
            _LOGGER.debug(
                "`%s` returned: %s", " ".join(cmd), ffx_target_show_info)

            return ffx_target_show_info
        except subprocess.TimeoutExpired as err:
            _LOGGER.debug(err, exc_info=True)
            raise
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def get_target_list(
            self,
            timeout: float = _TIMEOUTS["FFX_CLI"]) -> List[Dict[str, Any]]:
        """Executed and returns the output of `ffx --machine json target list`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx --machine json target list`.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        cmd: List[str] = _FFX_CMDS["TARGET_LIST"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)

            ffx_target_list_info: List[Dict[str, Any]] = json.loads(output)
            _LOGGER.debug(
                "`%s` returned: %s", " ".join(cmd), ffx_target_list_info)

            return ffx_target_list_info
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def get_target_name(self, timeout: float = _TIMEOUTS["FFX_CLI"]) -> str:
        """Returns the target name.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target name.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        # {
        #    "title": "Target",
        #    "label": "target",
        #    "description": "",
        #    "child": [
        #      {
        #        "title": "Name",
        #        "label": "name",
        #        "description": "Target name.",
        #        "value": "fuchsia-201f-3b5a-1c1b"
        #      },
        #      {
        #        "title": "SSH Address",
        #        "label": "ssh_address",
        #        "description": "Interface address",
        #        "value": "::1:8022"
        #      }
        #    ]
        #  },

        try:
            ffx_target_show_info: List[Dict[str,
                                            Any]] = self.get_target_information(
                                                timeout)
            target_entry: Dict[str, Any] = self._get_label_entry(
                ffx_target_show_info, label_value="target")
            name_entry: Dict[str, Any] = self._get_label_entry(
                target_entry["child"], label_value="name")
            return name_entry["value"]
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"Failed to get the target name of {self._target}") from err

    def get_target_ssh_address(
            self,
            timeout: float = _TIMEOUTS["FFX_CLI"]
    ) -> custom_types.TargetSshAddress:
        """Returns the target's ssh ip address and port information.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            (Target SSH IP Address, Target SSH Port)

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        cmd: List[str] = _FFX_CMDS["TARGET_SSH_ADDRESS"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)
            output = output.strip()
            _LOGGER.debug("`%s` returned: %s", " ".join(cmd), output)

            # in '[fe80::6a47:a931:1e84:5077%qemu]:22', ":22" is SSH port.
            # Ports can be 1-5 chars, clip off everything after the last ':'.
            ssh_info: List[str] = output.rsplit(":", 1)
            ssh_ip: str = ssh_info[0].replace("[", "").replace("]", "")
            ssh_port: int = int(ssh_info[1])

            return custom_types.TargetSshAddress(ip=ssh_ip, port=ssh_port)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def get_target_type(self, timeout: float = _TIMEOUTS["FFX_CLI"]) -> str:
        """Returns the target type.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target type.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        # Sample ffx_target_show_info containing product type (board):
        # [
        #     {
        #         'title': 'Build',
        #         'label': 'build',
        #         'description': '',
        #         'child': [
        #             {
        #                 'title': 'Version',
        #                 'label': 'version',
        #                 'description': 'Build version.',
        #                 'value': '2023-02-01T17:26:40+00:00'
        #             },
        #             {
        #                 'title': 'Product',
        #                 'label': 'product',
        #                 'description': 'Product config.',
        #                 'value': 'workstation_eng'
        #             },
        #             {
        #                 'title': 'Board',
        #                 'label': 'board',
        #                 'description': 'Board config.',
        #                 'value': 'qemu-x64'
        #             },
        #             {
        #                 'title': 'Commit',
        #                 'label': 'commit',
        #                 'description': 'Integration Commit Date',
        #                 'value': '2023-02-01T17:26:40+00:00'
        #             }
        #         ]
        #     },
        # ]
        target_show_info: List[Dict[str, Any]] = self.get_target_information(
            timeout=timeout)
        build_entry: Dict[str, Any] = self._get_label_entry(
            target_show_info, label_value="build")
        board_entry: Dict[str, Any] = self._get_label_entry(
            build_entry["child"], label_value="board")
        return board_entry["value"]

    def is_target_connected(
            self, timeout: float = _TIMEOUTS["FFX_CLI"]) -> bool:
        """Checks if target is connected to the host according to FFX.

            * If device name shows up under `ffx target list` with `rcs_state`
              set to 'Y' then treat target is online / connected.
            * If device name does not shows up under `ffx target list` (or)
              shows up but with `rcs_state` not set to Y then treat target is
              offline / not-connected.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            True if target is connected, False otherwise.
        """
        try:
            target_list: List[Dict[str, Any]] = self.get_target_list(
                timeout=timeout)
        except Exception as err:  # pylint: disable=broad-except
            _LOGGER.warning(err)
            return False

        for target in target_list:
            if target["nodename"] == self._target and \
               target["rcs_state"] == "Y":
                return True
        return False

    def run(
            self,
            cmd: List[str],
            timeout: float = _TIMEOUTS["FFX_CLI"],
            exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
    ) -> str:
        """Executes and returns the output of `ffx -t {target} {cmd}`.

        Args:
            cmd: FFX command to run.
            timeout: Timeout to wait for the ffx command to return.
            exceptions_to_skip: Any non fatal exceptions to be ignored.

        Returns:
            Output of `ffx -t {target} {cmd}`.

        Raises:
            subprocess.TimeoutExpired: In case of timeout
            errors.FfxCommandError: In case of failure.
        """
        exceptions_to_skip = tuple(exceptions_to_skip or [])

        ffx_cmd: List[str] = FFX._generate_ffx_cmd(cmd=cmd, target=self._target)
        try:
            _LOGGER.debug("Executing command `%s`", " ".join(ffx_cmd))
            output: str = subprocess.check_output(
                ffx_cmd, stderr=subprocess.STDOUT, timeout=timeout).decode()

            _LOGGER.debug("`%s` returned: %s", " ".join(ffx_cmd), output)

            return output
        except Exception as err:  # pylint: disable=broad-except
            if isinstance(err, exceptions_to_skip):
                return ""

            if isinstance(err, subprocess.TimeoutExpired):
                _LOGGER.debug(err, exc_info=True)
                raise

            raise errors.FfxCommandError(f"`{ffx_cmd}` command failed") from err

    # List all private methods in alphabetical order
    @staticmethod
    def _generate_ffx_args(target: Optional[str]) -> List[str]:
        """Generates all the arguments that need to be used with FFX command.

        Returns:
            List of FFX arguments.
        """
        ffx_args: List[str] = []

        # Do not change this sequence
        if target:
            ffx_args.extend(["-t", f"{target}"])

        # To run FFX in isolation mode
        if _ISOLATE_DIR:
            ffx_args.extend(["--isolate-dir", _ISOLATE_DIR])

        # To collect FFX logs
        if _LOGS_DIR:
            logs_config: Dict[str, Any] = {
                "log": {
                    "dir": _LOGS_DIR,
                    "level": "debug"
                }
            }
            ffx_args.extend(["--config", json.dumps(logs_config)])

        return ffx_args

    @staticmethod
    def _generate_ffx_cmd(cmd: List[str], target: Optional[str]) -> List[str]:
        """Generates the FFX command that need to be passed
        subprocess.check_output.

        Args:
            cmd: FFX command.

        Returns:
            FFX command to be run as list of string.
        """
        ffx_args: List[str] = FFX._generate_ffx_args(target)
        return ["ffx"] + ffx_args + cmd

    def _get_label_entry(self, data: List[Dict[str, Any]],
                         label_value: str) -> Dict[str, Any]:
        """Find and return ("label", label_value) entry in (list of dict) data
        provided.

        If a match is found, returns the corresponding dictionary entry from the
        list. Otherwise returns an empty dict.

        Args:
            data: Input data.
            label_value: Label value

        Returns:
            Dictionary matching the search criteria.
        """
        for entry in data:
            if entry.get("label") == label_value:
                return entry
        return {}
