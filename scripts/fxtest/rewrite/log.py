# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import collections
from collections.abc import Iterator
from dataclasses import dataclass
import gzip
import json
import math
import sys
import typing
import zlib

import environment
import event


async def writer(
    recorder: event.EventRecorder,
    out_stream: typing.TextIO,
) -> None:
    """Asynchronously serialize events to the given stream.

    Args:
        recorder (event.EventRecorder): The source of events to
            drain. Continues until all events are written.
        out_stream (typing.TextIO): Output text stream.
    """
    value: event.Event

    async for value in recorder.iter():
        try:
            json.dump(value.to_dict(), out_stream)  # type:ignore
            out_stream.write("\n")
            # Eagerly flush after each line. This task may terminate at any time,
            # including from an interrupt, so this ensures we at least see
            # the most recently written lines.
            out_stream.flush()
        except TypeError as e:
            print(f"LOG ERROR: {e} {value}")


@dataclass
class LogIterElement:
    # If set, this element provides the path of the log that was opened.
    log_path: str | None = None

    # If set, this elements provides one event from the file.
    log_event: event.Event | None = None

    # If set, this element provides a warning message. Parsing should continue.
    warning: str | None = None

    # If set, this element provides a fatal error message. Parsing should stop.
    error: str | None = None


class LogSource:
    __static_key = object()

    def __init__(
        self,
        __static_key: typing.Any,
        exec_env: environment.ExecutionEnvironment | None = None,
        stream: typing.TextIO | None = None,
    ):
        assert (
            __static_key == self.__static_key
        ), "LogSource must be created by from_env() or from_stream()."
        assert (
            exec_env is None or stream is None
        ), "LogSource must be either an environment or stream."
        self._exec_env = exec_env
        self._stream = stream

    @classmethod
    def from_env(
        cls, exec_env: environment.ExecutionEnvironment
    ) -> "LogSource":
        return LogSource(cls.__static_key, exec_env=exec_env)

    @classmethod
    def from_stream(cls, stream: typing.TextIO | None = None) -> "LogSource":
        return LogSource(cls.__static_key, stream=stream)

    def read_log(self) -> Iterator[LogIterElement]:
        stream = self._stream

        try:
            if stream is None and self._exec_env is not None:
                log_path = self._exec_env.get_most_recent_log()
                yield LogIterElement(log_path=log_path)
                stream = gzip.open(log_path, "rt")

            assert stream is not None

            for line in stream:
                if not line:
                    continue

                json_contents = json.loads(line)
                log_event: event.Event = event.Event.from_dict(json_contents)  # type: ignore[attr-defined]
                yield LogIterElement(log_event=log_event)
        except environment.EnvironmentError as e:
            yield LogIterElement(error=f"Failed to read log: {e}")
            return
        except gzip.BadGzipFile as e:
            yield LogIterElement(
                error=f"File does not appear to be a gzip file. ({e})"
            )
            return
        except json.JSONDecodeError as e:
            yield LogIterElement(
                warning=f"Found invalid JSON data, skipping the rest and proceeding. ({e})"
            )
        except (EOFError, zlib.error) as e:
            yield LogIterElement(
                warning=f"File may be corrupt, skipping the rest and proceeding. ({e})",
            )


def pretty_print(
    log_source: LogSource,
) -> bool:
    suite_names: dict[int, str] = dict()
    command_to_suite: dict[int, int] = dict()

    formatted_suite_events: collections.defaultdict[
        int, list[str]
    ] = collections.defaultdict(list)

    time_base: float | None = None

    def format_time(e: event.Event) -> str:
        assert time_base
        ts = e.timestamp - time_base
        seconds = math.floor(ts)
        millis = int(ts * 1e3 % 1e3)
        return f"{seconds:04}.{millis:03}"

    for element in log_source.read_log():
        if (e := element.log_event) is not None:
            if e.id == 0:
                time_base = e.timestamp
            if not e.payload:
                continue
            if ex := e.payload.test_suite_started:
                assert e.id
                suite_names[e.id] = ex.name
                formatted_suite_events[e.id].append(
                    f"[{format_time(e)}] Starting suite {ex.name}"
                )
            if (
                (pid := e.parent)
                and pid in suite_names
                and (command := e.payload.program_execution)
            ):
                assert e.id
                command_to_suite[e.id] = pid
                args = " ".join([command.command] + command.flags)
                env = command.environment
                formatted_suite_events[pid].append(
                    f"[{format_time(e)}] Running command\n  Args: {args}\n   Env: {env}"
                )
            if e.id in command_to_suite:
                if output := e.payload.program_output:
                    formatted_suite_events[command_to_suite[e.id]].append(
                        f"[{format_time(e)}] {output.data}"
                    )
                if termination := e.payload.program_termination:
                    formatted_suite_events[command_to_suite[e.id]].append(
                        f"[{format_time(e)}] Command terminated: {termination.return_code}"
                    )
            if e.id in suite_names and (outcome := e.payload.test_suite_ended):
                formatted_suite_events[e.id].append(
                    f"[{format_time(e)}] Suite ended with status {outcome.status.value}"
                )
        elif (warning := element.warning) is not None:
            print(warning, file=sys.stderr)
        elif (error := element.error) is not None:
            print(error, file=sys.stderr)
            return False
    print(f"{len(suite_names)} tests were run")
    for id in sorted(suite_names.keys()):
        name = suite_names[id]
        print(f"\n[START {name}]")
        for line in formatted_suite_events[id]:
            print(line.strip())
        print(f"[END {name}]\n")
    return True
