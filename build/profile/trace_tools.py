#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Common functions for working with trace formatting.
"""

import contextlib
from pathlib import Path
from typing import Any, Callable, Dict, Generator, Iterable, Optional

_SCRIPT_BASENAME = Path(__file__).name


def event_json(
    name: str, category: str, time: int, value_type: str, value: Any
) -> str:
    """Formats JSON for a single trace event's value."""
    return f"""{{"name": "{name}", "cat": "{category}", "ph": "C", "pid": 1, "tid": 1, "ts": {time}, "args": {{"{value_type}": "{value}"}} }},"""


class Formatter(object):
    def __init__(self) -> None:
        self._indent_spaces = 0

    @property
    def indent(self) -> str:
        return " " * self._indent_spaces

    @contextlib.contextmanager
    def indent_section(self) -> Generator[None, None, None]:
        self._indent_spaces += 2
        yield
        self._indent_spaces -= 2


def _stream_section(
    formatter: Formatter,
    generator: Callable[[Formatter], Iterable[str]],
    open: str,
    close: str,
    key: Optional[str] = None,
) -> Iterable[str]:
    """Prints a stream of sub-objects as enclosed lines, even if interrupted."""
    if key:
        yield formatter.indent + f'"{key}": {open}'
    else:
        yield formatter.indent + open

    try:
        with formatter.indent_section():
            yield from generator(formatter)

    # cleanly close the trace, even if interrupted
    finally:
        yield formatter.indent + close


def stream_array(
    formatter: Formatter,
    generator: Callable[[Formatter], Iterable[str]],
    key: Optional[str] = None,
    trailing_comma: bool = False,
) -> Iterable[str]:
    yield from _stream_section(
        formatter,
        generator,
        open="[",
        close="]," if trailing_comma else "]",
        key=key,
    )


def stream_dictionary(
    formatter: Formatter,
    generator: Callable[[Formatter], Iterable[str]],
    key: Optional[str] = None,
    trailing_comma: bool = False,
) -> Iterable[str]:
    yield from _stream_section(
        formatter,
        generator,
        open="{",
        close="}," if trailing_comma else "}",
        key=key,
    )


def stream_trace_events(
    formatter: Formatter, generator: Callable[[Formatter], Iterable[str]]
) -> Iterable[str]:
    """Chrome trace format using JSON objects expects "traceEvents"."""
    yield from stream_array(
        formatter, generator, key="traceEvents", trailing_comma=True
    )


def _key_value_formatter(fmt: Formatter, k: str, v: str) -> str:  # single-line
    # For dictionary entries, need trailing comma.
    return f'{fmt.indent}"{k}": "{v}",'


def _metadata_dict_formatter(
    fmt: Formatter, d: Dict[str, str]
) -> Iterable[str]:
    for k, v in d.items():
        yield _key_value_formatter(fmt, k, v)


def stream_trace(
    formatter: Formatter,
    metadata: Dict[str, str],
    event_generator: Callable[[Formatter], Iterable[str]],
) -> Iterable[str]:
    """Emit a complete trace of events with metadata, JSON object format.

    If interrupted, the output still closes properly as well-formed JSON.

    Output is semi-formatted in that indentation is correct, but each
    trace entry is crammed onto one line without wrapping.

    Args:
      formatter: Formatter to assist with indentation.
      metadata: additional information about the build invocation/environment.
      event_generator: generates the main event trace payload.
    """

    def metadata_formatter(fmt: Formatter) -> Iterable[str]:
        yield from _metadata_dict_formatter(fmt, metadata)

    def _main(fmt: Formatter) -> Iterable[str]:
        yield from stream_dictionary(
            fmt, metadata_formatter, key="otherData", trailing_comma=True
        )

        yield from stream_trace_events(fmt, event_generator)

    yield from stream_dictionary(formatter, _main)


def metadata_arg_to_dict(metadata_arg: str) -> Dict[str, str]:
    d: Dict[str, str] = {}
    if not metadata_arg:
        return d
    pairs = metadata_arg.split(",")
    for p in pairs:
        k, sep, v = p.partition(":")
        if sep != ":":
            raise ValueError(f"Expected a 'key:value', but got '{p}'.")
        d[k] = v
    return d
