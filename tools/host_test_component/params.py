# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os

from dataclasses import dataclass
from typing import Self

from execution_params import ExecutionParams

SDK_TOOL_PATH_KEY = "FUCHSIA_SDK_TOOL_PATH"
TARGETS_KEY = "FUCHSIA_TARGETS"
OUTPUT_DIRECTORY_KEY = "FUCHSIA_TEST_OUTDIR"
EXECUTION_JSON_KEY = "FUCHSIA_EXECUTION_JSON"


@dataclass
class Params:
    """Class to parse and validate environment parameters."""

    sdk_tool_path: str
    target: str
    execution_params: ExecutionParams
    output_directory: str

    @classmethod
    def initialize(cls, env_vars: dict) -> Self:
        """This function initializes this class using environment dictionary.

        Args:
            env_vars (dict): Dictionary containing required environment variables.

        Raises:
            ValueError: Validation error

        Returns:
            Self
        """
        sdk_tool_path = env_vars.get(SDK_TOOL_PATH_KEY)
        output_directory = env_vars.get(OUTPUT_DIRECTORY_KEY)
        execution_json = env_vars.get(EXECUTION_JSON_KEY)

        if sdk_tool_path is None:
            raise ValueError(
                f"'{SDK_TOOL_PATH_KEY}' environment variable is not available."
            )

        if output_directory is None:
            raise ValueError(
                f"'{OUTPUT_DIRECTORY_KEY}' environment variable is not available."
            )

        if not os.path.exists(sdk_tool_path):
            raise ValueError(
                f"'{SDK_TOOL_PATH_KEY}: {sdk_tool_path}' path does not exist."
            )

        if not os.path.exists(output_directory):
            raise ValueError(
                f"'{OUTPUT_DIRECTORY_KEY}: {output_directory}' path does not exist."
            )

        if execution_json is None:
            raise ValueError(
                f"'{EXECUTION_JSON_KEY}' environment variable is not available."
            )

        execution_params = ExecutionParams.initialize_from_json(execution_json)

        return cls(
            sdk_tool_path,
            env_vars.get(TARGETS_KEY, None),
            execution_params,
            output_directory,
        )
