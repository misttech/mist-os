#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains methods specific to FFX CLI."""

import json
import subprocess
from typing import Any, Dict, List

from honeydew import errors

_CMDS = {
    "FUCHSIA_TARGETS_SHOW":
        "ffx -t {target} --timeout {timeout} target show --json",
}

_TIMEOUTS = {
    "FFX_CLI": 10,
}


# List all the public methods in alphabetical order
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
        FfxCommandError: In case of failure.
    """
    try:
        cmd = _CMDS["FUCHSIA_TARGETS_SHOW"].format(
            target=target, timeout=timeout)
        output = subprocess.check_output(
            cmd, shell=True, stderr=subprocess.STDOUT).decode()
        ffx_target_show_info: List[Dict[str, Any]] = json.loads(output)
        return ffx_target_show_info
    except Exception as err:
        raise errors.FfxCommandError(
            f"`ffx -t {target} target show` command failed") from err


def get_target_address(
        target: str, timeout: float = _TIMEOUTS["FFX_CLI"]) -> str:
    """Returns the target ip address.

    Args:
        target: Target name.
        timeout: Timeout to wait for the ffx command to return.

    Returns:
        Target IP address.

    Raises:
        FfxCommandError: In case of failure.
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
        ffx_target_show_info = ffx_target_show(target, timeout)
        target_entry = _get_label_entry(
            ffx_target_show_info, label_value="target")
        ssh_address_entry = _get_label_entry(
            target_entry["child"], label_value="ssh_address")
        # in 'fe80::92bf:167b:19c3:58f0%qemu:22', ":22" is SSH port.
        return ssh_address_entry['value'][:-3]
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
        FfxCommandError: In case of failure.
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
        ffx_target_show_info = ffx_target_show(target, timeout)
        build_entry = _get_label_entry(
            ffx_target_show_info, label_value="build")
        board_entry = _get_label_entry(
            build_entry["child"], label_value="board")
        return board_entry['value']
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(
            f"Failed to get the target type of {target}") from err


# List all private methods in alphabetical order
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
