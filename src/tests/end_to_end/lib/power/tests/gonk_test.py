#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for the perf metric publishing code."""

import math
import tempfile
import unittest

from power import gonk
from trace_processing import trace_model, trace_time


class GonkSampleTest(unittest.TestCase):
    """Tests for Gonk sample parsing."""

    HOST_TIME = "20240927 18:00:23.609548"
    HOST_TIME_IN_MICROSECONDS = 1727460023609548  # No timezone, i.e. GMT.

    VALUES_WITH_DATA = [
        HOST_TIME,
        # Delta from last sample in microseconds.
        "286",
        # Voltage measurements.
        "0.8148437500000001",
        "1.010546875",
        "4.894726562500001",
        "1.0718750000000001",
        "0.9806640625",
        "3.3820312500000003",
        "1.8125",
        # Current measurements.
        "0.5274375",
        "0.052025",
        "0.6104375000000001",
        "0.2362578125",
        "1.2365162037037039",
        "0.3898229166666667",
        "0.005085",
        # Power measurements.
        "0.42977915039062503",
        "0.052573701171875",
        "2.9879246459960944",
        "0.25323884277343756",
        "1.2126070036711518",
        "1.3183932861328127",
        "0.0092165625",
        "",
    ]

    VALUES_WITH_COMMENT = [HOST_TIME, "0", "", "", "", "Header pin assert: 2"]

    def test_parse_sample_from_values_with_data(self) -> None:
        start = trace_time.TimePoint(0)
        sample = gonk.GonkSample.from_values(
            GonkSampleTest.VALUES_WITH_DATA, start
        )
        self.assertEqual(
            sample.host_time.to_epoch_delta().to_microseconds(),
            GonkSampleTest.HOST_TIME_IN_MICROSECONDS,
        )
        self.assertEqual(
            sample.gonk_time.to_epoch_delta().to_microseconds(), 286
        )
        self.assertListEqual(
            sample.voltages,
            [
                0.8148437500000001,
                1.010546875,
                4.894726562500001,
                1.0718750000000001,
                0.9806640625,
                3.3820312500000003,
                1.8125,
            ],
        )
        self.assertListEqual(
            sample.currents,
            [
                0.5274375,
                0.052025,
                0.6104375000000001,
                0.2362578125,
                1.2365162037037039,
                0.3898229166666667,
                0.005085,
            ],
        )
        self.assertListEqual(
            sample.powers,
            [
                0.42977915039062503,
                0.052573701171875,
                2.9879246459960944,
                0.25323884277343756,
                1.2126070036711518,
                1.3183932861328127,
                0.0092165625,
            ],
        )
        self.assertIsNone(sample.pin_assert)

    def test_parse_sample_from_values_with_comment(self) -> None:
        start = trace_time.TimePoint(123456)
        sample = gonk.GonkSample.from_values(
            GonkSampleTest.VALUES_WITH_COMMENT, start
        )
        self.assertEqual(
            sample.host_time.to_epoch_delta().to_microseconds(),
            GonkSampleTest.HOST_TIME_IN_MICROSECONDS,
        )
        self.assertEqual(sample.gonk_time, start)
        self.assertEqual(len(sample.voltages), 1)
        self.assertEqual(len(sample.currents), 1)
        self.assertEqual(len(sample.powers), 1)
        self.assertTrue(math.isnan(sample.voltages[0]))
        self.assertTrue(math.isnan(sample.currents[0]))
        self.assertTrue(math.isnan(sample.powers[0]))
        self.assertEqual(sample.pin_assert, 2)


class GonkMergeTest(unittest.TestCase):
    """Tests for merging Gonk samples into a trace."""

    TRACE_PID = 0
    TRACE_TID = 1
    RAIL_NAMES = ["rail0", "rail1"]

    @classmethod
    def _create_gpio_trace_event(cls, nanoseconds: int) -> trace_model.Event:
        return trace_model.Event(
            category="gpio",
            name="set-low",
            start=trace_time.TimePoint(nanoseconds),
            pid=cls.TRACE_PID,
            tid=cls.TRACE_TID,
            args={},
        )

    @classmethod
    def _create_gpio_gonk_sample(cls, nanoseconds: int) -> gonk.GonkSample:
        n_rails = len(cls.RAIL_NAMES)
        return gonk.GonkSample(
            # For now, just assume that host and Gonk times are perfectly synchronized.
            host_time=trace_time.TimePoint(nanoseconds),
            gonk_time=trace_time.TimePoint(nanoseconds),
            delta=trace_time.TimeDelta(nanoseconds),
            voltages=[float("nan")] * n_rails,
            currents=[float("nan")] * n_rails,
            powers=[float("nan")] * n_rails,
            pin_assert=2,
        )

    @classmethod
    def _create_trace_model(
        cls, trace_events: list[trace_model.Event]
    ) -> trace_model.Model:
        thread = trace_model.Thread(
            cls.TRACE_TID,
            "test_thread",
            trace_events,
        )
        proc = trace_model.Process(
            cls.TRACE_PID, "test_process", threads=[thread]
        )
        model = trace_model.Model()
        model.processes.append(proc)
        return model

    def test_merge_gonk_data_with_trace_events_reversed(self) -> None:
        gpio_times = [0, 60 * 1000 * 1000 * 1000]  # In nanoseconds.
        trace_events = []
        gonk_samples = []
        for t in gpio_times:
            trace_events.append(self._create_gpio_trace_event(t))
            gonk_samples.append(self._create_gpio_gonk_sample(t))
        trace_events.reverse()

        model = self._create_trace_model(trace_events)
        # TODO(https://fxbug.dev/381134672): Properly construct the trace file
        # and validate its contents after merging Gonk data.
        trace_file = tempfile.NamedTemporaryFile()
        gonk.merge_gonk_data(
            model, gonk_samples, trace_file.name, self.RAIL_NAMES
        )

    def test_merge_gonk_data_with_gonk_samples_reversed(self) -> None:
        gpio_times = [0, 60 * 1000 * 1000 * 1000]  # In nanoseconds.
        trace_events = []
        gonk_samples = []
        for t in gpio_times:
            trace_events.append(self._create_gpio_trace_event(t))
            gonk_samples.append(self._create_gpio_gonk_sample(t))
        gonk_samples.reverse()

        model = self._create_trace_model(trace_events)
        # TODO(https://fxbug.dev/381134672): Properly construct the trace file
        # and validate its contents after merging Gonk data.
        trace_file = tempfile.NamedTemporaryFile()
        gonk.merge_gonk_data(
            model, gonk_samples, trace_file.name, self.RAIL_NAMES
        )
