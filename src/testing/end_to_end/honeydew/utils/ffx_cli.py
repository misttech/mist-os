#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains methods specific to FFX CLI."""

import json
import logging
import subprocess
import tempfile
from shutil import rmtree
from typing import Any, Dict, List, Optional

from honeydew import custom_types, errors

_FFX_CMDS: Dict[str, List[str]] = {
    "TARGET_SHOW": ["target", "show", "--json"],
}

_TIMEOUTS: Dict[str, float] = {
    "FFX_CLI": 10,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)

_ISOLATE_DIR: Optional[str] = None
_LOGS_DIR: Optional[str] = None


# List all the public methods in alphabetical order
def check_ffx_connection(
        target: str, timeout: float = _TIMEOUTS["FFX_CLI"]) -> bool:
    """Check if Host is able to communicate with the (Fuchsia) target via FFX.

    Args:
        target: Target name.
        timeout: Timeout to wait for the ffx command to return.

    Returns:
        True if successful, False otherwise.

    Raises:
        errors.FfxCommandError: In case of failure.
    """
    # Sample ffx_target_show_info containing ssh_address:
    # [
    #     {
    #         'title': 'Target',
    #         'label': 'target',
    #         'description': '',
    #         'child': [
    #             {
    #                 'title': 'Name',
    #                 'label': 'name',
    #                 'description': 'Target name.',
    #                 'value': 'fuchsia-emulator'
    #             },
    #             {
    #                 'title': 'SSH Address',
    #                 'label': 'ssh_address',
    #                 'description': 'Interface address',
    #                 'value': 'fe80::92bf:167b:19c3:58f0%qemu:22'
    #             }
    #         ]
    #     },
    # ]
    try:
        ffx_target_show_info: List[Dict[str, Any]] = ffx_target_show(
            target, timeout)
    except subprocess.TimeoutExpired:
        # which means fuchsia device is not currently connected to host
        return False
    target_entry: Dict[str, Any] = _get_label_entry(
        ffx_target_show_info, label_value="target")
    target_title_entry: Dict[str, Any] = _get_label_entry(
        target_entry["child"], label_value="name")
    return target_title_entry["value"] == target


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


def ffx_target_show(
        target: str,
        timeout: float = _TIMEOUTS["FFX_CLI"]) -> List[Dict[str, Any]]:
    """Returns the output of `ffx -t {target} target show`.

    Args:
        target: Target name.
        timeout: Timeout to wait for the ffx command to return.

    Returns:
        Output of `ffx -t {target} target show`.

    Raises:
        subprocess.TimeoutExpired: In case of timeout
        errors.FfxCommandError: In case of failure.
    """
    cmd: List[str] = _generate_ffx_cmd(
        target=target, cmd=_FFX_CMDS["TARGET_SHOW"])
    try:
        _LOGGER.debug("Executing command `%s`", " ".join(cmd))
        output: str = subprocess.check_output(
            cmd, stderr=subprocess.STDOUT, timeout=timeout).decode()

        ffx_target_show_info: List[Dict[str, Any]] = json.loads(output)
        _LOGGER.debug("`%s` returned: %s", " ".join(cmd), ffx_target_show_info)

        return ffx_target_show_info
    except subprocess.TimeoutExpired as err:
        _LOGGER.debug(err, exc_info=True)
        raise
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(f"`{cmd}` command failed") from err


def get_target_ssh_address(
        target: str,
        timeout: float = _TIMEOUTS["FFX_CLI"]) -> custom_types.TargetSshAddress:
    """Returns the target's ssh ip address and port information.

    Args:
        target: Target name.
        timeout: Timeout to wait for the ffx command to return.

    Returns:
        (Target SSH IP Address, Target SSH Port)

    Raises:
        errors.FfxCommandError: In case of failure.
    """
    # Sample ffx_target_show_info containing ssh_address:
    # [
    #     {
    #         'title': 'Target',
    #         'label': 'target',
    #         'description': '',
    #         'child': [
    #             {
    #                 'title': 'Name',
    #                 'label': 'name',
    #                 'description': 'Target name.',
    #                 'value': 'fuchsia-emulator'
    #             },
    #             {
    #                 'title': 'SSH Address',
    #                 'label': 'ssh_address',
    #                 'description': 'Interface address',
    #                 'value': 'fe80::92bf:167b:19c3:58f0%qemu:22'
    #             }
    #         ]
    #     },
    # ]
    try:
        ffx_target_show_info: List[Dict[str, Any]] = ffx_target_show(
            target, timeout)
        target_entry: Dict[str, Any] = _get_label_entry(
            ffx_target_show_info, label_value="target")
        ssh_address_entry: Dict[str, Any] = _get_label_entry(
            target_entry["child"], label_value="ssh_address")
        # in 'fe80::92bf:167b:19c3:58f0%qemu:22', ":22" is SSH port.
        # Ports can be 1-5 characters, clip off everything after the last ':'.
        ssh_info: List[str] = ssh_address_entry["value"].rsplit(":", 1)
        return custom_types.TargetSshAddress(
            ip=ssh_info[0], port=int(ssh_info[1]))
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(
            f"Failed to get the ip address of {target}") from err


def get_target_type(target: str, timeout: float = _TIMEOUTS["FFX_CLI"]) -> str:
    """Returns the target type.

    Args:
        target: Target name.
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
    try:
        ffx_target_show_info: List[Dict[str, Any]] = ffx_target_show(
            target, timeout)
        build_entry: Dict[str, Any] = _get_label_entry(
            ffx_target_show_info, label_value="build")
        board_entry: Dict[str, Any] = _get_label_entry(
            build_entry["child"], label_value="board")
        return board_entry["value"]
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(
            f"Failed to get the target type of {target}") from err


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


# List all private methods in alphabetical order
def _generate_ffx_args(target: str) -> List[str]:
    """Generates all the arguments that need to be used with FFX command.

    Args:
        target: Target name.

    Returns:
        List of FFX arguments.
    """
    ffx_args: List[str] = []

    # Do not change this sequence
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


def _generate_ffx_cmd(target: str, cmd: List[str]) -> List[str]:
    """Generates the FFX command that need to be passed subprocess.check_output.

    Args:
        target: Target name.
        cmd: FFX command.

    Returns:
        FFX command to be run as list of string.
    """
    ffx_args: List[str] = _generate_ffx_args(target)
    return ["ffx"] + ffx_args + cmd


def _get_label_entry(data: List[Dict[str, Any]],
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
