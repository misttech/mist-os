#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for cpu_breakdown.py."""

from trace_processing.metrics import cpu_breakdown
from typing import List
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import unittest


class CpuBreakdownTest(unittest.TestCase):
    """CPU breakdown tests."""

    def construct_trace_model(self) -> trace_model.Model:
        model = trace_model.Model()
        threads: List[trace_model.Thread] = []
        for i in range(1, 5):
            threads.append(trace_model.Thread(i, "thread-%d" % i))
        model.processes = [
            # Process with PID 1000 and threads with TIDs 1, 2, 3, 4.
            trace_model.Process(1000, "big_process", threads),
            # Process with PID 2000 and thread with TID 100.
            trace_model.Process(
                2000, "small_process", [trace_model.Thread(100, "small-thread")]
            ),
        ]

        # CPU 2.
        records_2: List[trace_model.SchedulingRecord] = [
            # "thread-1" is active from 500-1000 ms, 500 total duration.
            trace_model.Waking(trace_time.TimePoint(100000000), 2, 612, {}),
            trace_model.ContextSwitch(
                trace_time.TimePoint(500000000),
                1,
                100,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(1000000000),
                100,
                1,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "small-thread" is active from 1000 to 2000 ms, 1000 total duration.
            trace_model.ContextSwitch(
                trace_time.TimePoint(2000000000),
                1,
                100,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "thread-2" is active from 3000-9000 ms, 6000 total duration.
            # Switch the order of adding to records - shouldn't change behavior.
            trace_model.ContextSwitch(
                trace_time.TimePoint(9000000000),
                100,
                2,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(3000000000),
                2,
                70,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.Waking(trace_time.TimePoint(3000000000), 2, 612, {}),
        ]

        # CPU 3.
        records_3: List[trace_model.SchedulingRecord] = [
            # "thread-3" (incoming) is idle; don't log it.
            trace_model.ContextSwitch(
                trace_time.TimePoint(3000000000),
                3,
                70,
                trace_model.INT32_MIN,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "thread-3" (outgoing) is idle; don't log it.
            trace_model.ContextSwitch(
                trace_time.TimePoint(4000000000),
                70,
                3,
                trace_model.INT32_MIN,
                trace_model.INT32_MIN,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # TID 70 (outgoing) is idle; don't log it.
            # "thread-2" (incoming) is not idle.
            trace_model.ContextSwitch(
                trace_time.TimePoint(5000000000),
                2,
                70,
                612,
                trace_model.INT32_MIN,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "thread-2" (outgoing) is not idle; log it.
            trace_model.ContextSwitch(
                trace_time.TimePoint(6000000000),
                3,
                2,
                trace_model.INT32_MIN,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
        ]

        # CPU 5.
        records_5: List[trace_model.SchedulingRecord] = []
        # Add 5 Waking and ContextSwitch events for "thread-1" where the incoming_tid
        # is 1 and the outgoing_tid (70) is not mapped to a Thread in our Process.
        for i in range(1, 6):
            records_5.append(
                trace_model.Waking(
                    trace_time.TimePoint(i * 1000000000 - 500000000),
                    1,
                    3122,
                    {},
                )
            )
            records_5.append(
                trace_model.ContextSwitch(
                    trace_time.TimePoint(i * 1000000000 - 500000000),
                    1,
                    70,
                    3122,
                    3122,
                    trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                    {},
                )
            )

            # Add 5 Waking events for the tid = 70 Thread.
            records_5.append(
                trace_model.Waking(
                    trace_time.TimePoint(i * 1000000000), 70, 3122, {}
                )
            )
            # Add 5 ContextSwitch events for "thread-1" where the outgoing_tid is 1.
            # "thread-1" is active from i*500 to i*1000, i.e. 500 ms increments. This happens 5 times = 2500 ms.
            records_5.append(
                trace_model.ContextSwitch(
                    trace_time.TimePoint(i * 1000000000),
                    70,
                    1,
                    3122,
                    3122,
                    trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                    {},
                )
            )

        model.scheduling_records = {2: records_2, 3: records_3, 5: records_5}
        return model

    def test_process_metrics(self) -> None:
        model = self.construct_trace_model()

        self.assertEqual(model.processes[0].name, "big_process")
        self.assertEqual(len(model.processes[0].threads), 4)
        self.assertEqual(model.processes[1].name, "small_process")
        self.assertEqual(len(model.processes[1].threads), 1)

        processor = cpu_breakdown.CpuBreakdownMetricsProcessor(model)
        breakdown = processor.process_metrics()

        self.assertEqual(len(breakdown), 3)

        # Each process: thread has the correct numbers for each CPU.
        self.assertEqual(
            breakdown["big_process: thread-1"], {2: 500.0, 5: 2500.0}
        )
        self.assertEqual(
            breakdown["big_process: thread-2"], {2: 6000.0, 3: 1000.0}
        )
        self.assertEqual(breakdown["small_process: small-thread"], {2: 1000.0})
        # If thread is idle, don't log it.
        self.assertNotIn("big_process: thread-3", breakdown)
        # If there is no duration for the thread, don't log it.
        self.assertNotIn("big_process: thread-4", breakdown)
