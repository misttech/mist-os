# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from enum import Enum
from typing import Callable

import fx_cmd
from async_utils.command import (
    AsyncCommand,
    CommandOutput,
    StderrEvent,
    StdoutEvent,
)


class FfxOutputFormat(Enum):
    TEXT = 0
    JSON = 1
    PRETTY_JSON = 2


class FfxCmd(fx_cmd.ExecutableCommand):
    def __init__(
        self,
        inner: fx_cmd.ExecutableCommand | None = None,
        output_format: FfxOutputFormat = FfxOutputFormat.TEXT,
    ):
        """Wrapper for executing ffx commands through fx.

        Args:
            inner (ExecutableCommand, optional): Configured command wrapper. If unset, use a default FxCmd.
            output_format (FfxOutputFormat, optional): Output format for ffx. Defaults to FfxOutputFormat.TEXT.
        """
        self._ffx_prefix_args: list[str] = []
        match output_format:
            case FfxOutputFormat.TEXT:
                pass
            case FfxOutputFormat.JSON:
                self._ffx_prefix_args = ["--machine", "json"]
            case FfxOutputFormat.PRETTY_JSON:
                self._ffx_prefix_args = ["--machine", "json-pretty"]
            case _:
                assert False, f"Unknown output format {output_format}"

        if inner is None:
            inner = fx_cmd.FxCmd()
        self._inner: fx_cmd.ExecutableCommand = inner

    def command_line(self, *args: str) -> list[str]:
        """Format a command line with ffx-specific settings.

        Returns:
            list[str]: Formatted command line.
        """
        return ["ffx"] + self._ffx_prefix_args + list(args)

    # overrides base class
    async def start(self, *args: str) -> AsyncCommand:
        return await self._inner.start(*self.command_line(*args))

    # overrides base class
    def sync(
        self,
        *args: str,
        stdout_callback: Callable[[StdoutEvent], None] | None = None,
        stderr_callback: Callable[[StderrEvent], None] | None = None,
    ) -> CommandOutput:
        return self._inner.sync(
            *self.command_line(*args),
            stdout_callback=stdout_callback,
            stderr_callback=stderr_callback,
        )
