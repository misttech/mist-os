#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for ../metrics/power.py."""

import unittest
from trace_processing.metrics import power
from trace_processing import trace_metrics, trace_model, trace_time
from typing import Iterable

# Boilerplate-busting constants:
U = trace_metrics.Unit
TCR = trace_metrics.TestCaseResult


class PowerMetricsTest(unittest.TestCase):
    """Power metrics tests."""

    def construct_trace_model(
        self, loadgen_tids: Iterable[int]
    ) -> trace_model.Model:
        """Builds a fake trace model.

        Args:
            loadgen_tids: The load generator process will have threads with these IDs.

        The fake returned by this method contains a load generator process with the specified
        number of threads, using the provided tids. It also contains a fake process containing
        power data held in CounterEvents. Voltage is always 12V.

        The timeline of power events is:
        500000µs, 1000mA --- 750000µs, 2000mA --- 1000000µs, 100mA --- 1250000, 600mA

        Tests should populate `model.scheduling_records` in order to orchestrate the desired
        interactions between load generation and other threads on the system.
        """
        event_template = {
            "cat": "Metrics",
            "name": "Metrics",
            "pid": 0x8C01_1EC7_EDDA_7A10,
            "tid": 0x8C01_1EC7_EDDA_7A20,
        }

        fake_power_process = trace_model.Process(
            0x8C01_1EC7_EDDA_7A10,
            "PowerData",
            [
                trace_model.Thread(
                    0x8C01_1EC7_EDDA_7A20,
                    "Fake",
                    [
                        trace_model.CounterEvent.from_dict(
                            {  # during sync signal
                                **event_template,
                                "ts": 500000,  # microseconds
                                "args": {"Voltage": 12, "Current": 1000},
                            }
                        ),
                        trace_model.CounterEvent.from_dict(
                            {  # during sync signal
                                **event_template,
                                "ts": 750000,  # microseconds
                                "args": {"Voltage": 12, "Current": 2000},
                            }
                        ),
                        trace_model.CounterEvent.from_dict(
                            {
                                **event_template,
                                "ts": 1000000,  # microseconds
                                "args": {"Voltage": 12, "Current": 100},
                            }
                        ),
                        trace_model.CounterEvent.from_dict(
                            {
                                **event_template,
                                "ts": 1250000,  # microseconds
                                "args": {"Voltage": 12, "Current": 600},
                            }
                        ),
                    ],
                ),
            ],
        )

        model = trace_model.Model()
        threads = [trace_model.Thread(i, f"thread-{i}") for i in loadgen_tids]
        model.processes = [
            # load_generator process with PID 1000 and threads with TIDs 1, 2.
            trace_model.Process(1000, "load_generator.cm", threads),
            fake_power_process,
        ]
        return model

    def test_process_metrics(self) -> None:
        """Correctly exclude power readings occurring during synchronization."""
        threads = (1, 2)
        model = self.construct_trace_model(threads)

        records_0: list[trace_model.SchedulingRecord] = [
            # "thread-1" is active from 0 - 1000, then exits.
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                threads[0],
                100,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(1000000000),
                70,
                threads[0],
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
        ]

        records_1: list[trace_model.SchedulingRecord] = [
            # small-thread
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                100,
                70,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
        ]
        model.scheduling_records = {0: records_0, 1: records_1}

        results = power.PowerMetricsProcessor().process_metrics(model)
        # Power samples should start to count the instant load generation stops, so expect
        # to count the .1A and .6A sample
        expected_results = [
            TCR(label="MinPower_by_model", unit=U.watts, values=[1.2]),
            TCR(label="MeanPower_by_model", unit=U.watts, values=[4.2]),
            TCR(label="MaxPower_by_model", unit=U.watts, values=[7.2]),
        ]
        self.assertEqual(expected_results, results)

    def test_sync_multithread(self) -> None:
        """Detect sync happening across multiple CPUs."""
        (t_1, t_2, t_3) = (1, 2, 3)
        model = self.construct_trace_model((t_1, t_2, t_3))

        records_0: list[trace_model.SchedulingRecord] = [
            # "thread-1" is active from 0 - 500, then exits.
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                t_1,
                100,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "thread-3" starts waiting to run on this processor at 450
            trace_model.Waking(trace_time.TimePoint(450000000), t_3, 612, {}),
            # "thread-3" takes over from 500 - 1000, then exits
            trace_model.ContextSwitch(
                trace_time.TimePoint(500000000),
                t_3,
                t_1,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(1000000000),
                9999,
                t_3,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
        ]

        records_1: list[trace_model.SchedulingRecord] = [
            # Some other thread is active from 0 - 250, then blocks.
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                9999,
                8888,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            # "thread-2" is active from 250 - 750, then exits
            trace_model.ContextSwitch(
                trace_time.TimePoint(250000000),
                t_2,
                9999,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(750000000),
                8888,
                t_2,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
        ]
        model.scheduling_records = {0: records_0, 1: records_1}

        results = power.PowerMetricsProcessor().process_metrics(model)
        expected_results = [
            TCR(label="MinPower_by_model", unit=U.watts, values=[1.2]),
            TCR(label="MeanPower_by_model", unit=U.watts, values=[4.2]),
            TCR(label="MaxPower_by_model", unit=U.watts, values=[7.2]),
        ]
        self.assertEqual(expected_results, results)

    def test_sync_gets_descheduled(self) -> None:
        """Detect sync getting descheduled in the middle and then coming back."""
        t_1 = 1
        model = self.construct_trace_model([t_1])

        records_0: list[trace_model.SchedulingRecord] = [
            # "thread-1" is active from 0 - 500, 750-1000, then exits.
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                t_1,
                8888,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(500000000),
                8888,
                t_1,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(750000000),
                t_1,
                8888,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(1000000000),
                70,
                t_1,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
            # "thread-1" starts waiting to run on this processor at 700
            trace_model.Waking(trace_time.TimePoint(700000000), t_1, 612, {}),
        ]

        records_1: list[trace_model.SchedulingRecord] = [
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                100,
                70,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
        ]
        model.scheduling_records = {0: records_0, 1: records_1}

        results = power.PowerMetricsProcessor().process_metrics(model)
        # Power samples should start to count the instant load generation stops, so expect
        # to count the .1A and .6A sample
        expected_results = [
            TCR(label="MinPower_by_model", unit=U.watts, values=[1.2]),
            TCR(label="MeanPower_by_model", unit=U.watts, values=[4.2]),
            TCR(label="MaxPower_by_model", unit=U.watts, values=[7.2]),
        ]
        self.assertEqual(expected_results, results)

    def test_no_sync_signal(self) -> None:
        """Detect sync not being present."""
        model = self.construct_trace_model([])

        records_0: list[trace_model.SchedulingRecord] = [
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                10,
                30,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
            trace_model.ContextSwitch(
                trace_time.TimePoint(1000000000),
                70,
                30,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_DEAD,
                {},
            ),
        ]

        records_1: list[trace_model.SchedulingRecord] = [
            trace_model.ContextSwitch(
                trace_time.TimePoint(0),
                40,
                70,
                612,
                612,
                trace_model.ThreadState.ZX_THREAD_STATE_BLOCKED,
                {},
            ),
        ]
        model.scheduling_records = {0: records_0, 1: records_1}
        self.assertEqual(
            [], power.PowerMetricsProcessor().process_metrics(model)
        )
