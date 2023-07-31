# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from dataclasses import dataclass
import datetime
import os
import typing

import args


class EnvironmentError(Exception):
    """There was an error loading the execution environment."""


_Self = typing.TypeVar("_Self", bound="ExecutionEnvironment")


@dataclass
class ExecutionEnvironment:
    """Contains the parsed environment for this invocation of fx test.

    The environment provides paths to the Fuchsia source directory, output
    directory, input files, and output files.
    """

    # The Fuchsia source directory, from the FUCHSIA_DIR environment variable.
    fuchsia_dir: str

    # The output build directory for compiled Fuchsia code.
    out_dir: str

    # Path to the log file to write to. If unset, do not log.
    log_file: typing.Optional[str]

    # Path to the input tests.json file.
    test_json_file: str

    # Path to the input test-list.json file.
    test_list_file: str

    @classmethod
    def initialize_from_args(cls: typing.Type[_Self], flags: args.Flags) -> _Self:
        """Initialize an execution environment from the given flags.

        Args:
            flags (args.Flags): Parsed command line flags.

        Raises:
            EnvironmentError: If the environment is not valid for some reason.

        Returns:
            ExecutionEnvironment: The processed environment for execution.
        """
        fuchsia_dir = os.getenv("FUCHSIA_DIR")
        if not fuchsia_dir or not os.path.isdir(fuchsia_dir):
            raise EnvironmentError(
                "Expected a directory in environment variable FUCHSIA_DIR"
            )

        # Get the build directory.
        # We could use fx status, but it's slow to execute now. We
        # don't actually need all of the status contents to find the
        # build directory, it is stored at this file path in the root
        # Fuchsia directory during build time.
        build_dir_file = os.path.join(fuchsia_dir, ".fx-build-dir")
        if not os.path.isfile(build_dir_file):
            raise EnvironmentError(f"Expected file .fx-build-dir at {build_dir_file}")
        with open(build_dir_file) as f:
            out_dir = os.path.join(fuchsia_dir, f.readline().strip())
        if not os.path.isdir(out_dir):
            raise EnvironmentError(f"Expected directory at {out_dir}")

        # Either disable logging, log to the given path, or format
        # a default path in the output directory.
        # We will write gzipped logs since they can get a bit large
        # and compress very well.
        log_file = (
            None
            if not flags.log
            else flags.logpath
            if flags.logpath
            else os.path.join(
                out_dir, f"fxtest-{datetime.datetime.now().isoformat()}.log.json.gz"
            )
        )

        # Get the input files from their expected locations directly
        # under the output directory.
        tests_json_file = os.path.join(out_dir, "tests.json")
        test_list_file = os.path.join(out_dir, "test-list.json")
        if not os.path.isfile(tests_json_file):
            raise EnvironmentError(f"Expected a file at {tests_json_file}")
        if not os.path.isfile(test_list_file):
            raise EnvironmentError(f"Expected a file at {test_list_file}")
        return cls(fuchsia_dir, out_dir, log_file, tests_json_file, test_list_file)

    def relative_to_root(self, path: str) -> str:
        """Return the path to a file relative to the Fuchsia directory.

        This is used to format paths like "/home/.../fuchsia/src/my_lib" as
        "//src/my_lib".

        Args:
            path (str): Absolute path under the Fuchsia directory.

        Returns:
            str: Relative path from the Fuchsia directory to the
                same destination.
        """
        return os.path.relpath(path, self.fuchsia_dir)
