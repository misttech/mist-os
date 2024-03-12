# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import collections
import json
import math
import sys
import typing
import zlib

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


def pretty_print(in_stream: typing.TextIO) -> None:
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

    try:
        for line in in_stream:
            if not line:
                continue
            json_contents = json.loads(line)
            e: event.Event = event.Event.from_dict(json_contents)  # type: ignore[attr-defined]
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
    except json.JSONDecodeError as e:
        print(
            f"Found invalid JSON data, skipping the rest and proceeding. ({e})",
            file=sys.stderr,
        )
    except (EOFError, zlib.error) as e:
        print(
            f"File may be corrupt, skipping the rest and proceeding. ({e})",
            file=sys.stderr,
        ),

    print(f"{len(suite_names)} tests were run")
    for id in sorted(suite_names.keys()):
        name = suite_names[id]
        print(f"\n[START {name}]")
        for line in formatted_suite_events[id]:
            print(line.strip())
        print(f"[END {name}]\n")
