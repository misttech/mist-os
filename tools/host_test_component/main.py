#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import os
import sys
import tempfile

from typing import Iterable

from command import Command
from command_runner import run_command
from params import Params


def handle_stdout(line: bytes):
    """Print output to stdout

    Args:
        output_stream (bytes): Bytes to print to stdout.
    """
    print(line)


def handle_stderr(line: bytes):
    """Print error to stderr

    Args:
        error_stream (bytes): Bytes to print to stderr.
    """
    print(line, file=sys.stderr)


def main():
    params = Params.initialize(os.environ)
    command = Command.initialize(params)
    with tempfile.TemporaryDirectory() as iso_dir:
        cmd = command.get_command(iso_dir)
        return_code = run_command(cmd, handle_stdout, handle_stderr)
        return return_code


if __name__ == "__main__":
    sys.exit(main())
