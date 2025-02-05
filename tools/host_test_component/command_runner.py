# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import logging
import sys
from typing import Callable, List

import async_utils
import async_utils.command

Handler = Callable[[bytes], None]

logger = logging.getLogger(__name__)


class CommandError(Exception):
    pass


def run_command(
    cmd: List[str],
) -> int:
    """Execute command and collect output

    Args:
        cmd (List[str]): Command to execute

    Returns:
        int: Exit code of the process
    """

    async def inner_async_command() -> int:
        logger.info(f"Running command: {cmd}")
        process = await async_utils.command.AsyncCommand.create(*cmd)

        async for event in process:
            if isinstance(event, async_utils.command.StdoutEvent):
                sys.stdout.write(event.text.decode())
                sys.stdout.flush()
            elif isinstance(event, async_utils.command.StderrEvent):
                sys.stderr.write(event.text.decode())
                sys.stderr.flush()
            elif isinstance(event, async_utils.command.TerminationEvent):
                logger.info(
                    f"Command ended after {event.runtime} seconds, return code {event.return_code}"
                )
                return event.return_code
        return -1

    try:
        return asyncio.run(inner_async_command())
    except async_utils.command.AsyncCommandError as e:
        raise CommandError(e)
