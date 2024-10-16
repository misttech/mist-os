#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Converts vmstat output to various trace formats.

Expects vmstat to have been invoked with -t for the timestamp column.

Usage:
  vmstat.py [options] INPUT > OUTPUT
"""

import argparse
import dataclasses
import datetime
import sys
from pathlib import Path
from typing import Any, Iterable, Iterator, Sequence

_SCRIPT_BASENAME = Path(__file__).name


@dataclasses.dataclass
class ProcessCounts:
    running: int
    blocked: int


@dataclasses.dataclass
class MemoryUsage:
    swap: int
    free: int
    buffers: int
    cache: int
    # inactive: int
    # active: int


@dataclasses.dataclass
class SwapRates:
    bytes_in_per_second: int
    bytes_out_per_second: int


@dataclasses.dataclass
class BlockIORates:
    received_per_second: int
    sent_per_second: int


@dataclasses.dataclass
class SystemEventRates:
    interrupts_per_second: int
    context_switches_per_second: int


@dataclasses.dataclass
class CPUUsage:
    user: int
    system: int
    idle: int
    wait_io: int
    stolen: int
    kvm_guest: int


def event_json(
    name: str, category: str, time: int, value_type: str, value: Any
) -> str:
    """Formats JSON for a single event's value."""
    return f"""{{"name": "{name}", "cat": "{category}", "ph": "C", "pid": 1, "tid": 1, "ts": {time}, "args": {{"{value_type}": "{value}"}} }},"""


@dataclasses.dataclass
class VmstatEntry:
    """Represents one line of output from 'vmstat -t'"""

    processes: ProcessCounts
    memory: MemoryUsage
    swap: SwapRates
    block_io: BlockIORates
    system: SystemEventRates
    cpu: CPUUsage
    timestamp: datetime.datetime

    def chrome_trace_events_json(
        self, start_time: datetime.datetime
    ) -> Iterable[str]:
        """Yields a set of trace events at a single time."""
        tdelta_us = int(
            (self.timestamp - start_time) / datetime.timedelta(microseconds=1)
        )

        def event(name: str, value_type: str, value: Any) -> str:
            return event_json(name, "system", tdelta_us, value_type, value)

        yield event("processes.running", "count", self.processes.running)
        yield event("processes.blocked", "count", self.processes.blocked)

        yield event("memory.swap", "bytes", self.memory.swap)
        yield event("memory.free", "bytes", self.memory.free)
        yield event("memory.buffers", "bytes", self.memory.buffers)
        yield event("memory.cache", "bytes", self.memory.cache)

        yield event(
            "swap.in", "bytes_per_second", self.swap.bytes_in_per_second
        )
        yield event(
            "swap.out", "bytes_per_second", self.swap.bytes_out_per_second
        )

        yield event(
            "block.in", "bytes_per_second", self.block_io.received_per_second
        )
        yield event(
            "block.out", "bytes_per_second", self.block_io.sent_per_second
        )

        yield event(
            "system.interrupts",
            "count_per_second",
            self.system.interrupts_per_second,
        )
        yield event(
            "system.context_switches",
            "count_per_second",
            self.system.context_switches_per_second,
        )

        yield event("cpu.user", "percent", self.cpu.user)
        yield event("cpu.system", "percent", self.cpu.system)
        yield event("cpu.idle", "percent", self.cpu.idle)
        yield event("cpu.wait_io", "percent", self.cpu.wait_io)
        yield event("cpu.stolen", "percent", self.cpu.stolen)
        yield event("cpu.kvm_guest", "percent", self.cpu.kvm_guest)


def _parse_data_row(line: str, max_fields: int) -> VmstatEntry:
    d = line.split(maxsplit=max_fields)
    timestamp_text = " ".join(d[18:])
    return VmstatEntry(
        processes=ProcessCounts(
            running=int(d[0]),  # "r"
            blocked=int(d[1]),  # "b"
        ),
        memory=MemoryUsage(
            swap=int(d[2]),  # "swpd"
            free=int(d[3]),  # "free"
            buffers=int(d[4]),  # "buff"
            cache=int(d[5]),  # "cache"
        ),
        swap=SwapRates(
            bytes_in_per_second=int(d[6]),  # "si"
            bytes_out_per_second=int(d[7]),  # "so"
        ),
        block_io=BlockIORates(
            received_per_second=int(d[8]),  # "bi"
            sent_per_second=int(d[9]),  # "bo"
        ),
        system=SystemEventRates(
            interrupts_per_second=int(d[10]),  # "in"
            context_switches_per_second=int(d[11]),  # "cs"
        ),
        cpu=CPUUsage(
            user=int(d[12]),  # "us"
            system=int(d[13]),  # "sy"
            idle=int(d[14]),  # "id"
            wait_io=int(d[15]),  # "wa"
            stolen=int(d[16]),  # "st"
            kvm_guest=int(d[17]),  # "gu"
        ),
        timestamp=datetime.datetime.strptime(
            timestamp_text, "%Y-%m-%d %H:%M:%S"
        ),  # "UTC"
    )


def _parse_vmstat_output(lines: Iterable[str]) -> Iterator[VmstatEntry]:
    num_fields = 18
    for line in lines:
        stripped_line = line.strip()
        if not stripped_line:
            continue
        if stripped_line.startswith("#"):  # comment
            continue
        if stripped_line.startswith("procs"):  # header categories
            continue
        if stripped_line.startswith(
            "r  b"
        ):  # header fields, starting with running/blocked processes
            # treat consecutive whitespace as single separator
            fields = stripped_line.split()
            num_fields = len(fields)
            continue
        # else is a line of trace data
        yield _parse_data_row(stripped_line, num_fields)


def print_chrome_trace_json(trace: Iterator[VmstatEntry]) -> Iterable[str]:
    yield "["

    try:
        try:
            first: VmstatEntry = next(trace)
        except StopIteration:
            # if trace is empty, abort
            return

        start_time = first.timestamp
        for line in first.chrome_trace_events_json(start_time):
            yield f"  {line}"

        # The remainder
        for t in trace:
            for line in t.chrome_trace_events_json(start_time):
                yield f"  {line}"

    # cleanly close the trace
    finally:
        yield "]"


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        # positional argument
        "input",
        type=Path,
        help="text file of 'vmstat -t' output.  Pass '-' to read from stdin.",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)
    if args.input == Path("-"):
        vmstat_lines = sys.stdin  # is Iterable[str]
    else:
        vmstat_lines = args.input.read_text().splitlines()

    trace = _parse_vmstat_output(vmstat_lines)

    for line in print_chrome_trace_json(trace):
        print(line)

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
