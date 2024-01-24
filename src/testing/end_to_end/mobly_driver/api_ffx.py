#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all FFX CLI APIs used in Mobly Driver."""

import ipaddress
import json
import subprocess
from dataclasses import dataclass
from typing import Any


_REMOTE_TARGET_SSH_PORT = 8022


class CommandException(Exception):
    """Raised when the FFX command fails."""


class OutputFormatException(Exception):
    """Raised when the FFX command output format is invalid."""


@dataclass(frozen=True)
class TargetListResult:
    """FFX target list output representation.

    Args:
      all_nodes: list of all discovered Fuchsia targets.
      default_nodes: list of default Fuchsia targets.
    """

    all_nodes: list[str]
    default_nodes: list[str]


@dataclass(frozen=True)
class TargetSshAddress:
    """Dataclass that holds IP Address and Port

    Args:
        ip: Ip Address
        port: Port Number
    """

    ip: ipaddress.IPv4Address | ipaddress.IPv6Address
    port: int | None

    def __str__(self) -> str:
        host: str = f"{self.ip}"
        if isinstance(self.ip, ipaddress.IPv6Address):
            host = f"[{host}]"
        if self.port:
            return f"{host}:{self.port}"
        else:
            return host

    def is_remote(self) -> bool:
        """Return True if fuchsia device is connected remotely, else False."""
        # "remote" target will have loopback ip address and 8022 as SSH port
        return self.ip.is_loopback and self.port == _REMOTE_TARGET_SSH_PORT


class FfxClient:
    def __init__(self, ffx_path: str):
        self._ffx_path = ffx_path

    def target_list(self, isolate_dir: str | None) -> TargetListResult:
        """Returns detected Fuchsia targets.

        Args:
          isolate_dir: If provided, FFX isolate dir to run command in.

        Returns:
          A TargetListResult object.

        Raises:
          CommandException if FFX command fails.
          OutputFormatException if unable to parse JSON result or JSON
            does not contain expected keys.
        """
        try:
            cmd = [self._ffx_path]

            if isolate_dir is not None:
                cmd += ["--isolate-dir", isolate_dir]

            cmd += [
                "--machine",
                "json",
                "target",
                "list",
            ]
            output = subprocess.check_output(cmd, timeout=5).decode()
        except (
            subprocess.CalledProcessError,
            subprocess.TimeoutExpired,
        ) as e:
            raise CommandException(
                f"Failed to enumerate devices via {cmd}: {e}"
            )

        try:
            output_json = json.loads(output)
            all_nodes = [t["nodename"] for t in output_json]
            default_nodes = [
                t["nodename"] for t in output_json if t["is_default"]
            ]
        except (json.JSONDecodeError, KeyError) as e:
            raise OutputFormatException(
                f"Failed to decode output from {cmd}: {e}"
            )

        return TargetListResult(all_nodes, default_nodes)

    def get_target_ssh_address(
        self, target_name: str, isolate_dir: str | None
    ) -> TargetSshAddress:
        """Returns the target's ssh ip address and port information.

        Args:
          target_name: Name of the fuchsia target
          isolate_dir: If provided, FFX isolate dir to run command in.

        Returns:
          A TargetSshAddress object.

        Raises:
          CommandException if FFX command fails.
          OutputFormatException if unable to parse command output.
        """
        try:
            cmd = [self._ffx_path]

            if isolate_dir is not None:
                cmd += ["--isolate-dir", isolate_dir]

            cmd += [
                "-t",
                target_name,
                "target",
                "get-ssh-address",
            ]
            output = subprocess.check_output(cmd, timeout=5).decode().strip()
        except (
            subprocess.CalledProcessError,
            subprocess.TimeoutExpired,
        ) as e:
            raise CommandException(
                f"Failed to get the target ssh address via {cmd}: {e}"
            )

        try:
            # output format: '[fe80::6a47:a931:1e84:5077%qemu]:22'
            ssh_info = output.rsplit(":", 1)
            ssh_ip = ipaddress.ip_address(
                ssh_info[0].replace("[", "").replace("]", "")
            )
            ssh_port = int(ssh_info[1])
        except ValueError as e:
            raise OutputFormatException(
                f"Failed to parse output from {cmd}: {e}"
            )

        return TargetSshAddress(ssh_ip, ssh_port)
