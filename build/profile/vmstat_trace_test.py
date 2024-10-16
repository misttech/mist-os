#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import datetime
import unittest
from pathlib import Path
from unittest import mock

import vmstat_trace


class EventJsonTests(unittest.TestCase):
    def test_basic(self) -> None:
        text = vmstat_trace.event_json("marbles", "fun", 5000, "count", 12)
        self.assertEqual(
            text,
            """{"name": "marbles", "cat": "fun", "ph": "C", "pid": 1, "tid": 1, "ts": 5000, "args": {"count": "12"} },""",
        )


_TEST_START_TIME = datetime.datetime(
    year=1984,
    month=11,
    day=5,
    hour=9,
    minute=30,
    second=0,
)

# this value matches _SAMPLE_VMSTAT_ENTRY
_SAMPLE_VMSTAT_DATA_LINE = (
    " 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 1984-11-05 9:30:00"
)

# this value matches _SAMPLE_VMSTAT_DATA_LINE
_SAMPLE_VMSTAT_ENTRY = vmstat_trace.VmstatEntry(
    processes=vmstat_trace.ProcessCounts(
        running=1,
        blocked=2,
    ),
    memory=vmstat_trace.MemoryUsage(
        swap=3,
        free=4,
        buffers=5,
        cache=6,
    ),
    swap=vmstat_trace.SwapRates(
        bytes_in_per_second=7,
        bytes_out_per_second=8,
    ),
    block_io=vmstat_trace.BlockIORates(
        received_per_second=9,
        sent_per_second=10,
    ),
    system=vmstat_trace.SystemEventRates(
        interrupts_per_second=11,
        context_switches_per_second=12,
    ),
    cpu=vmstat_trace.CPUUsage(
        user=13,
        system=14,
        idle=15,
        wait_io=16,
        stolen=17,
        kvm_guest=18,
    ),
    timestamp=_TEST_START_TIME,
)


class VmstatEntryTests(unittest.TestCase):
    def test_chrome_trace_events_json(self) -> None:
        events = list(
            _SAMPLE_VMSTAT_ENTRY.chrome_trace_events_json(_TEST_START_TIME)
        )
        # There are 18 fields of vmstat output (not counting timestamp).
        self.assertEqual(len(events), 18)

    def test_parse_data_row(self) -> None:
        entry = vmstat_trace._parse_data_row(_SAMPLE_VMSTAT_DATA_LINE, 18)
        self.assertEqual(entry, _SAMPLE_VMSTAT_ENTRY)

    def test_parse_vmstat_output(self) -> None:
        lines = [
            "# Remember, remember, the 5th of November",  # ignore comment
            "",  # ignore blank
            "procs -----------memory---------- ---swap-- -----io---- -system-- -------cpu------- -----timestamp-----",
            " r  b   swpd   free   buff  cache   si   so    bi    bo   in   cs us sy id wa st gu                 UTC",
            _SAMPLE_VMSTAT_DATA_LINE,
        ]
        entries = list(vmstat_trace._parse_vmstat_output(iter(lines)))
        self.assertEqual(entries, [_SAMPLE_VMSTAT_ENTRY])

    def test_print_chrome_trace_json_empty(self) -> None:
        lines = list(vmstat_trace.print_chrome_trace_json(iter([])))
        self.assertEqual(lines, ["[", "]"])

    def test_print_chrome_trace_json_nonempty(self) -> None:
        lines = list(
            vmstat_trace.print_chrome_trace_json(
                iter([_SAMPLE_VMSTAT_ENTRY] * 2)
            )
        )
        self.assertEqual(lines[0], "[")
        self.assertEqual(lines[-1], "]")
        self.assertEqual(len(lines[1:-1]), 18 * 2)  # one per field


class MainTests(unittest.TestCase):
    def test_parse_and_print(self) -> None:
        argv = ["vmstat.log"]
        with mock.patch.object(
            Path, "read_text", return_value=_SAMPLE_VMSTAT_DATA_LINE
        ) as mock_read:
            returncode = vmstat_trace.main(argv)
        self.assertEqual(returncode, 0)
        mock_read.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
