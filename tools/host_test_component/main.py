#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import logging
import os
import sys
import tempfile

from command import Command
from command_runner import CommandError, run_command
from params import Params

logger = logging.getLogger(__name__)


def main() -> int:
    logging.basicConfig(
        level=logging.DEBUG if "DEBUG" in os.environ else logging.WARNING
    )

    logger.debug(f"Environment: {os.environ}")
    logger.debug(f"Args: {sys.argv}")

    try:
        params = Params.initialize(dict(os.environ))
    except ValueError as e:
        logger.error(f"Failed to initialize parameters from environment: {e}")
        logger.debug(f"Environment")
        return 1

    command = Command.initialize(params)
    logger.info(f"Using these parameters: {params}")

    with tempfile.TemporaryDirectory() as iso_dir:
        cmd = command.get_command(iso_dir)
        try:
            return run_command(cmd)
        except CommandError as e:
            logger.error(f"Command failed: {e}")
            return 1


if __name__ == "__main__":
    sys.exit(main())
