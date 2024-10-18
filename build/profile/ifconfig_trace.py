#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Converts ifconfig (linux) output to various trace formats.

Specifically, this reads the output of ifconfig_loop.sh,
which calls ifconfig repeatedly between printing timestamps.
Non-linux variants of ifconfig are not supported.
Currently, this outputs in chrome-trace format only.

Usage:
  ifconfig_trace.py [options] INPUT > OUTPUT
"""

import argparse
import dataclasses
import datetime
import sys
from pathlib import Path
from typing import Any, Dict, Iterable, Iterator, Sequence

_SCRIPT_BASENAME = Path(__file__).name


def event_json(
    name: str, category: str, time: int, value_type: str, value: Any
) -> str:
    """Formats JSON for a single event's value."""
    return f"""{{"name": "{name}", "cat": "{category}", "ph": "C", "pid": 1, "tid": 1, "ts": {time}, "args": {{"{value_type}": "{value}"}} }},"""


@dataclasses.dataclass
class TransmissionData:
    interface_name: str
    tx_packets: int
    tx_bytes: int
    rx_packets: int
    rx_bytes: int
    timestamp: datetime.datetime

    def chrome_trace_events_json(
        self, start_time: datetime.datetime, prev: "TransmissionData"
    ) -> Iterable[str]:
        """Yields a set of trace events at a single time."""
        assert self.interface_name == prev.interface_name
        tdelta_us = int(
            (self.timestamp - start_time) / datetime.timedelta(microseconds=1)
        )

        def event(name: str, value_type: str, value: int) -> str:
            return event_json(
                f"{self.interface_name}.{name}",
                "network",
                tdelta_us,
                value_type,
                value,
            )

        # ifconfig tx/rx data is cumulative, so we need compute differences
        # since the last sample.
        yield event("rx.packets", "count", self.rx_packets - prev.rx_packets)
        yield event("rx.bytes", "count", self.rx_bytes - prev.rx_bytes)
        yield event("tx.packets", "count", self.tx_packets - prev.tx_packets)
        yield event("tx.bytes", "count", self.tx_bytes - prev.tx_bytes)


def _parse_ifconfig_loop_output(
    lines: Iterable[str],
) -> Iterator[TransmissionData]:
    # Lines need to be grouped together by time.
    # The format from ifconfig_loop.sh is as follows:
    #
    #   TIME: <timestamp>
    #   [ifconfig output]
    #
    # where ifconfig output looks like any number of:
    #   interface_name: ...
    #           data ...
    #           data ...
    #           ...
    #   <blank line, marking end of data>

    interface_name: str = ""
    rx_packets: int = 0
    rx_bytes: int = 0
    tx_packets: int = 0
    tx_bytes: int = 0
    current_time: datetime.datetime
    for line in lines:
        if line.startswith("TIME: "):
            current_time = datetime.datetime.strptime(
                line.strip().removeprefix("TIME: "), "%Y-%m-%d %H:%M:%S"
            )
            continue

        if not line.rstrip():
            # blank line marks the end of interface section
            yield TransmissionData(
                interface_name=interface_name,
                rx_packets=rx_packets,
                rx_bytes=rx_bytes,
                tx_packets=tx_packets,
                tx_bytes=tx_bytes,
                timestamp=current_time,
            )
            continue

        if not line.startswith(" "):
            # other unindented lines start an interface section
            interface_name, _, _ = line.partition(":")
            continue

        # the remaining lines are data for the last named interface
        stripped_line = line.strip()
        if stripped_line.startswith("RX packets"):
            # Looks like:
            #   RX packets ##packets##  bytes ##bytes## (### GiB)
            _, _, rx_packets_str, _, rx_bytes_str, _, _ = stripped_line.split()
            rx_packets = int(rx_packets_str)
            rx_bytes = int(rx_bytes_str)
        elif stripped_line.startswith("TX packets"):
            # Looks like:
            #   TX packets ##packets##  bytes ##bytes## (### GiB)
            _, _, tx_packets_str, _, tx_bytes_str, _, _ = stripped_line.split()
            tx_packets = int(tx_packets_str)
            tx_bytes = int(tx_bytes_str)

        # else: drop other data we don't care about


def print_chrome_trace_json(trace: Iterator[TransmissionData]) -> Iterable[str]:
    yield "["

    prev: Dict[str, TransmissionData] = {}
    try:
        try:
            first: TransmissionData = next(trace)
        except StopIteration:
            # if trace is empty, abort
            return

        # Keep track of previous by interface name, so we can report the
        # numeric differences.
        prev[first.interface_name] = first
        start_time = first.timestamp
        for line in first.chrome_trace_events_json(start_time, first):
            yield f"  {line}"

        # The remainder
        for t in trace:
            if t.interface_name not in prev:  # first occurrence
                prev[t.interface_name] = t
            for line in t.chrome_trace_events_json(
                start_time, prev[t.interface_name]
            ):
                yield f"  {line}"
            prev[t.interface_name] = t

    # cleanly close the trace
    finally:
        yield "]"


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        # positional argument
        "input",
        type=Path,
        help="output of 'ifconfig_loop.sh'.  Pass '-' to read from stdin.",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)
    if args.input == Path("-"):
        ifconfig_lines = sys.stdin  # is Iterable[str]
    else:
        ifconfig_lines = args.input.read_text().splitlines()

    trace = _parse_ifconfig_loop_output(ifconfig_lines)

    for line in print_chrome_trace_json(trace):
        print(line)

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
