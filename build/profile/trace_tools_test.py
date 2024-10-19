#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
from typing import Iterable

import trace_tools


class EventJsonTests(unittest.TestCase):
    def test_basic(self) -> None:
        text = trace_tools.event_json("marbles", "fun", 5000, "count", 12)
        self.assertEqual(
            text,
            """{"name": "marbles", "cat": "fun", "ph": "C", "pid": 1, "tid": 1, "ts": 5000, "args": {"count": "12"} },""",
        )


class FormatterTests(unittest.TestCase):
    def test_indent_section(self):
        fmt = trace_tools.Formatter()
        self.assertEqual(fmt.indent, "")
        with fmt.indent_section():
            self.assertEqual(fmt.indent, "  ")  # 2 spaces
            with fmt.indent_section():
                self.assertEqual(fmt.indent, "    ")  # 4 spaces

            self.assertEqual(fmt.indent, "  ")

        self.assertEqual(fmt.indent, "")


class StreamArrayTests(unittest.TestCase):
    def test_empty(self):
        fmt = trace_tools.Formatter()

        def empty(f: trace_tools.Formatter) -> Iterable[str]:
            yield from []

        lines = list(trace_tools.stream_array(fmt, empty))
        self.assertEqual(lines, ["[", "]"])

    def test_counts_unindented(self):
        fmt = trace_tools.Formatter()

        def counts(f: trace_tools.Formatter) -> Iterable[str]:
            # Intentionally forego formatting with proper indentation.
            yield from ["five", "six", "seven", "eight"]

        lines = list(trace_tools.stream_array(fmt, counts))
        self.assertEqual(lines, ["[", "five", "six", "seven", "eight", "]"])

    def test_counts_indented_and_named(self):
        fmt = trace_tools.Formatter()

        def counts(f: trace_tools.Formatter) -> Iterable[str]:
            yield from (f"{f.indent}{x}" for x in ["one", "two"])

        lines = list(
            trace_tools.stream_array(
                fmt, counts, key="values", trailing_comma=True
            )
        )
        self.assertEqual(lines, ['"values": [', "  one", "  two", "],"])


class StreamDictionaryTests(unittest.TestCase):
    def test_empty(self):
        fmt = trace_tools.Formatter()

        def empty(f: trace_tools.Formatter) -> Iterable[str]:
            yield from []

        lines = list(trace_tools.stream_dictionary(fmt, empty))
        self.assertEqual(lines, ["{", "}"])

    def test_counts_unindented(self):
        fmt = trace_tools.Formatter()

        def counts(f: trace_tools.Formatter) -> Iterable[str]:
            # Intentionally forego formatting with proper indentation.
            yield from ["four: 4", "five: 5", "six: 6"]

        lines = list(trace_tools.stream_dictionary(fmt, counts))
        self.assertEqual(lines, ["{", "four: 4", "five: 5", "six: 6", "}"])

    def test_counts_indented_and_named(self):
        fmt = trace_tools.Formatter()

        def counts(f: trace_tools.Formatter) -> Iterable[str]:
            yield from (f"{f.indent}{x}" for x in ["one: 1", "two: 2"])

        lines = list(
            trace_tools.stream_dictionary(
                fmt, counts, key="maps", trailing_comma=True
            )
        )
        self.assertEqual(lines, ['"maps": {', "  one: 1", "  two: 2", "},"])


class StreamTraceEventsTests(unittest.TestCase):
    def test_stream(self):
        fmt = trace_tools.Formatter()

        def events(f: trace_tools.Formatter) -> Iterable[str]:
            yield from (f"{f.indent}{x}," for x in ["event1", "event2"])

        lines = list(trace_tools.stream_trace_events(fmt, events))
        self.assertEqual(
            lines, ['"traceEvents": [', "  event1,", "  event2,", "],"]
        )


class StreamTraceTests(unittest.TestCase):
    def test_basic_outline(self):
        fmt = trace_tools.Formatter()

        def events(f: trace_tools.Formatter) -> Iterable[str]:
            yield from (f"{f.indent}{x}," for x in ["event1", "event2"])

        lines = list(
            trace_tools.stream_trace(
                fmt,
                metadata={"FOO": "bar"},
                event_generator=events,
            )
        )
        self.assertEqual(
            lines,
            [
                "{",
                '  "otherData": {',
                '    "FOO": "bar",',
                "  },",
                '  "traceEvents": [',
                "    event1,",
                "    event2,",
                "  ],",
                "}",
            ],
        )


class MetadataArgToDictTests(unittest.TestCase):
    def test_empty(self):
        d = trace_tools.metadata_arg_to_dict("")
        self.assertEqual(d, {})

    def test_one_value(self):
        d = trace_tools.metadata_arg_to_dict("FOO:bar")
        self.assertEqual(d, {"FOO": "bar"})

    def test_multiple_values(self):
        d = trace_tools.metadata_arg_to_dict("FOO:bar,BAZ:quux")
        self.assertEqual(d, {"FOO": "bar", "BAZ": "quux"})


if __name__ == "__main__":
    unittest.main()
