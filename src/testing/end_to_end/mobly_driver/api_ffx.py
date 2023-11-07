#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all FFX CLI APIs used in Mobly Driver."""

import json
import subprocess
import tempfile
from dataclasses import dataclass
from typing import Any


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
