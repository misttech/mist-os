# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os

from dataclasses import dataclass
from typing import List, Self

from params import Params


@dataclass
class Command:
    """Class for generating ffx command from 'Params'."""

    sdk_tool_path: str
    target: str | None
    test_url: str
    realm: str | None
    max_severity_logs: str | None
    test_args: List[str]
    test_filters: List[str]
    run_disabled_tests: bool
    parallel: str | None
    output_directory: str

    @classmethod
    def initialize(cls, params: Params) -> Self:
        """Initialize command from params.

        Args:
            params (Params): 'Params' object

        Returns:
            Self
        """
        ep = params.execution_params
        return cls(
            params.sdk_tool_path,
            params.target,
            ep.test_url,
            ep.realm,
            ep.max_severity_logs,
            ep.test_args,
            ep.test_filters,
            ep.run_disabled_tests,
            ep.parallel,
            params.output_directory,
        )

    def get_command(self, isolate_dir: str) -> List[str]:
        """Build and return the command to run.

        Args:
            isolate_dir (str): Directory to run the command in (if provided)

        Returns:
            List[str]: List representation of the final built command.
        """
        # TODO(anmittal): Support test filter.
        cmd = [os.path.join(self.sdk_tool_path, "ffx")]
        if isolate_dir:
            cmd.extend(["--isolate-dir", isolate_dir])
        if self.target:
            cmd.extend(["-t", self.target])
        cmd.extend(["test", "run", self.test_url])
        if self.realm:
            cmd.extend(["--realm", self.realm])
        if self.output_directory:
            cmd.extend(["--output-directory", self.output_directory])
        if self.max_severity_logs:
            cmd.extend(["--max-severity-logs", self.max_severity_logs])
        if self.parallel:
            cmd.extend(["--parallel", self.parallel])
        if self.run_disabled_tests:
            cmd.append("--run-disabled")
        return cmd
