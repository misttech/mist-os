# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Asynchronously run commands and read output as events.

This module provides the AsyncCommand class, which runs a command in a separate
process and converts stdout, stderr, and termination into typed events that
can be iterated over asynchronously.

asyncio supports spawning and monitoring subprocesses asynchronously, but
the official docs (https://docs.python.org/3.8/library/asyncio-subprocess.html)
include an ominous warning about awaiting on output streams directly.
Directly waiting on output streams can lead to a deadlock due to
output buffering, while relying on communicate() to read large
buffers data is not recommended.

AsyncCommand handles spawning individual async tasks to read stderr
and stdout concurrently.  Because asyncio internally connects these
pipes using non-blocking async readers, we do not hang.

"""

import asyncio
from dataclasses import dataclass
from dataclasses import field
from io import StringIO
import os
import signal
import time
import typing


@dataclass
class StdoutEvent:
    """This event represents data emitted to the program's stdout pipe."""

    # The text, as bytes.
    text: bytes

    # The monotonic timestamp this program received the content.
    timestamp: float = field(default_factory=lambda: time.monotonic())


@dataclass
class StderrEvent:
    """This event represents data emitted to the program's stderr pipe."""

    # The text, as bytes.
    text: bytes

    # The monotonic timestamp this program received the content.
    timestamp: float = field(default_factory=lambda: time.monotonic())


@dataclass
class TerminationEvent:
    """This event represents the termination of a subcommand."""

    # The return code for the program.
    return_code: int

    # The program's runtime, in seconds.
    runtime: float

    # If the output of the program was wrapped in another, this
    # value indicates the return code of the wrapper program
    wrapper_return_code: int | None = None

    # The monotonic timestamp this program received the termination event.
    timestamp: float = field(default_factory=lambda: time.monotonic())

    # If true, this command is terminating due to hitting its timeout.
    was_timeout: bool = False


class AsyncCommandError(Exception):
    """An error occurred starting or running a command asynchronously."""


@dataclass
class CommandOutput:
    """Summary information for an entire command execution."""

    # The stdout contents, as a UTF-8 string.
    stdout: str

    # The stderr contents, as a UTF-8 string.
    stderr: str

    # The return code for the program.
    return_code: int

    # The runtime for the program, in seconds.
    runtime: float

    # If this program's output was wrapped by another program, this is
    # the return code of that wrapper.
    wrapper_return_code: int | None

    # If True, this command was forced to terminate due to timeout.
    was_timeout: bool = False


CommandEvent = StdoutEvent | StderrEvent | TerminationEvent


class AsyncCommand:
    """Asynchronously executed subprocess command.

    This class implements an iterator over CommandEvent, which represents
    activity of the command over time. Note that at most one iterator
    may exist over the command events.
    """

    def __init__(
        self,
        process: asyncio.subprocess.Process,
        wrapped_process: asyncio.subprocess.Process | None = None,
        timeout: float | None = None,
    ):
        """Create an AsyncCommand that wraps a subprocess.

        The subprocess must have been created with both stdout and stderr
        set to "PIPE".

        This method should not be used directly, use AsyncCommand.create instead.

        Args:
            process (asyncio.subprocess.Process): The process to wrap.
            wrapped_process (asyncio.subprocess.Process): A secondary process to wrap, which must also be closed.
            timeout (float, optional): Terminate the process after this number of seconds.
        """
        self._process = process
        self._wrapped_process = wrapped_process
        self._event_queue: asyncio.Queue[CommandEvent] = asyncio.Queue(128)
        self._done_iterating = False
        self._start = time.time()
        self._runner_task = asyncio.create_task(self._task())
        self._timeout = timeout

    @classmethod
    async def create(
        cls,
        program: str,
        *args: str,
        symbolizer_args: typing.List[str] | None = None,
        env: typing.Dict[str, str] | None = None,
        timeout: float | None = None,
    ):
        """Create a new AsyncCommand that runs the given program.

        Args:
            program (str): Name of the program to run.
            args (List[str]): Arguments to pass to the program.
            symbolizer_args (Optional[List[str]]): If set, pipe output from the program through this program.
            env (Optional[Dict[str,str]]): If set, use this dict
                to populate the command's environment. Note that the
                CWD environment variable is handled specially to ensure
                that the command runs in the given working directory.
            timeout (float, optional): Terminate the command after the given number of seconds.

        Returns:
            AsyncCommand: A new AsyncCommand executing the process.

        Raises:
            AsyncCommandError: If there is a problem executing the process.
        """
        cwd = env["CWD"] if env and "CWD" in env else None
        if cwd and env:
            env.pop("CWD")
            if not env:
                env = None

        try:

            async def make_process(output_pipe: typing.Union[int, typing.TextIO]):
                return await asyncio.subprocess.create_subprocess_exec(
                    program,
                    *args,
                    stdout=output_pipe,
                    stderr=output_pipe,
                    env=env,
                    cwd=cwd,
                    preexec_fn=os.setpgrp,  # To support killing by pgid.
                )

            base_command = None
            if symbolizer_args:
                # Wrap the base command we want to run in another
                # command that executes the symbolizer. We need to explicitly
                # close our side of the pipes after passing to the relevant
                # subprocesses.

                read_pipe, write_pipe = os.pipe()

                base_command = await make_process(write_pipe)
                os.close(write_pipe)
                command = await asyncio.subprocess.create_subprocess_exec(
                    *symbolizer_args,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    stdin=read_pipe,
                    preexec_fn=os.setpgrp,  # To support killing by pgid.
                )
                os.close(read_pipe)
            else:
                command = await make_process(asyncio.subprocess.PIPE)

            return AsyncCommand(command, base_command, timeout=timeout)
        except FileNotFoundError as e:
            raise AsyncCommandError(f"The program '{e.filename}' was not found.")
        except Exception as e:
            raise AsyncCommandError(f"An unknown error occurred: {e}")

    def send_signal(self, sig: int):
        """Send the given signal to the underlying process(es) and their
        children in the same process group.

        Args:
            sig (int): The signal to send.
        """
        # We need to signal the entire process group we created, otherwise
        # we run the risk of signallying only a shell script and not
        # the processes run by that shell script (e.g. `fx`).
        pg = os.getpgid(self._process.pid)
        os.killpg(pg, sig)
        if self._wrapped_process is not None:
            pg = os.getpgid(self._wrapped_process.pid)
            os.killpg(pg, sig)

    def terminate(self):
        """Terminate the underlying process(es) by sending SIGTERM."""
        self.send_signal(signal.SIGTERM)

    def kill(self):
        """Immediately kill the underlying process(es) by sending SIGKILL."""
        self.send_signal(signal.SIGKILL)

    async def run_to_completion(
        self,
        callback: typing.Callable[[CommandEvent], None] | None = None,
    ) -> CommandOutput:
        """Runs the program to completion, collecting the resulting outputs.

        Args:
            callback (typing.Callable[[CommandEvent], None], optional): If set, send all
                CommandEvents to this callback function as they are produced.
                Defaults to None.
            timeout (float, optional): Terminate the command after this many seconds.

        Raises:
            AsyncCommandError: If an internal error causes the task queue to be cancelled.

        Returns:
            CommandOutput: Summary of the execution of the program.
        """
        with StringIO() as stdout, StringIO() as stderr:
            async for e in self:
                if callback:
                    callback(e)
                if isinstance(e, StdoutEvent):
                    text = e.text.decode("utf-8", errors="replace")
                    stdout.write(text)
                elif isinstance(e, StderrEvent):
                    text = e.text.decode("utf-8", errors="replace")
                    stderr.write(text)
                elif isinstance(e, TerminationEvent):
                    return CommandOutput(
                        stdout=stdout.getvalue(),
                        stderr=stderr.getvalue(),
                        return_code=e.return_code,
                        wrapper_return_code=e.wrapper_return_code,
                        runtime=e.runtime,
                        was_timeout=e.was_timeout,
                    )

        raise AsyncCommandError("Event stream unexpectedly stopped")

    async def _task(self):
        tasks = []

        async def read_stream(
            input_stream: asyncio.StreamReader,
            event_type: typing.Type[typing.Union[StderrEvent, StdoutEvent]],
        ):
            """Wrap reading from a stream and emitting a specific type of event.

            Args:
                input_stream (asyncio.StreamReader): Reader to read from.
                event_type: Either StdoutEvent or StderrEvent type, to wrap each line.
            """
            async for line in input_stream:
                await self._event_queue.put(event_type(text=line))

        async def timeout_handler(timeout: float, timeout_signal: asyncio.Event):
            """Handle timeouts.

            We first send SIGTERM after the timeout, and if the program has
            still not terminated after an additional delay we send SIGKILL.

            Args:
                timeout (float): The initial wait time before termination.
                timeout_signal (asyncio.Event): An event to set on timeout.
            """
            await asyncio.sleep(timeout)
            timeout_signal.set()
            self.terminate()
            # Give the process at most 5 seconds to terminate, or less
            # if timeout is less than 5 seconds.
            await asyncio.sleep(min(timeout, 5))
            self.kill()

        timeout_task: asyncio.Task | None = None
        did_timeout = asyncio.Event()
        if self._timeout is not None:
            timeout_task = asyncio.create_task(
                timeout_handler(self._timeout, did_timeout)
            )

        # Read stdout and stderr in parallel, forwarding the appropriate events.
        assert self._process.stdout
        assert self._process.stderr
        tasks.append(
            asyncio.create_task(read_stream(self._process.stdout, StdoutEvent))
        )
        tasks.append(
            asyncio.create_task(read_stream(self._process.stderr, StderrEvent))
        )

        # Wait for the process to exit and get its return code.
        return_code = await self._process.wait()
        if timeout_task and not timeout_task.done():
            # Cancel the timeout task so we do not try to send signals to a terminated process.
            timeout_task.cancel()
        wrapper_return_code: int | None = None
        if self._wrapped_process:
            # Also ensure we wait for the wrapped process, and use it's return code as the canonical code.
            wrapper_return_code = return_code
            return_code = await self._wrapped_process.wait()

        runtime = time.time() - self._start

        if timeout_task is not None:
            # Make sure the timeout task is cancelled and awaited
            # so it is cleaned up.
            timeout_task.cancel()
            tasks.append(timeout_task)

        # Wait for all output to drain before termination event.
        await asyncio.wait(tasks)
        await self._event_queue.put(
            TerminationEvent(
                return_code,
                runtime,
                wrapper_return_code=wrapper_return_code,
                was_timeout=did_timeout.is_set(),
            )
        )

    async def next_event(self) -> CommandEvent | None:
        """Return the next event from the process execution, if one exists.

        Returns:
            Event | None: The next event on the command, or None if the command is terminated.
        """
        if self._done_iterating:
            return None

        ret = await self._event_queue.get()
        if isinstance(ret, TerminationEvent):
            self._done_iterating = True
        return ret

    def __aiter__(self):
        return self

    async def __anext__(self):
        ret = await self.next_event()
        if ret is None:
            raise StopAsyncIteration()
        else:
            return ret
