# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import typing
from pathlib import Path

from async_utils.command import (
    AsyncCommand,
    CommandEvent,
    CommandOutput,
    StderrEvent,
    StdoutEvent,
)
from build_dir import get_build_directory


class FxCmd:
    """Wrapper for executing `fx` commands through Python.

    Usage (async):
        execution = FxCmd("build", timeout=30.0).start()
        result = await execution.run_to_completion()

    Usage (sync):
        def callback(line: StdoutEvent):
          print(line.text)
        FxCmd("build", timeout=30.0).sync(stdout_callback=callback)
    """

    def __init__(
        self,
        *args: str,
        build_directory: Path | None = None,
        timeout: float | None = None,
    ):
        """Entry-point for running `fx` commands.

        Args:
            build_directory (Path | None, optional): If set, use
                this as the build directory. Otherwise, determine build
                directory from environment.
            timeout (float | None, optional): Timeout for the command
                in seconds. Default is no timeout.
        """
        self._argss: list[str] = list(args)
        self._build_directory: Path | None = build_directory
        self._timeout: float | None = timeout

    @property
    def command_line(self) -> list[str]:
        """The formatted command line this command will execute."""
        build_directory = self._build_directory
        if build_directory is None:
            build_directory = get_build_directory()

        return ["fx", "--dir", str(build_directory)] + self._argss

    async def start(self) -> AsyncCommand:
        """Start an invocation of fx asynchronously.

        The returned command can be iterated over for output of the
        command, or run_to_completion can be used to get the final
        result of the called process.

        Returns:
            AsyncCommand: An asynchronously running invocation of fx.
        """
        args = self.command_line
        return await AsyncCommand.create(
            args[0], *args[1:], timeout=self._timeout
        )

    def sync(
        self,
        stdout_callback: typing.Callable[[StdoutEvent], None] | None = None,
        stderr_callback: typing.Callable[[StderrEvent], None] | None = None,
    ) -> CommandOutput:
        """Run an invocation of fx to completion synchronously

        Note that this method creates its own asyncio loop and will fail if
        it is called in the context of an existing asyncio loop. For async,
        use start() to get an AsyncCommand directly.

        Returns:
            CommandOutput: The result of running the command to completion.
        """

        def local_callback(event: CommandEvent) -> None:
            if stdout_callback is not None and isinstance(event, StdoutEvent):
                stdout_callback(event)
            elif stderr_callback is not None and isinstance(event, StderrEvent):
                stderr_callback(event)

        async def operation() -> CommandOutput:
            running_command = await self.start()
            return await running_command.run_to_completion(
                callback=local_callback
            )

        return asyncio.run(operation())
