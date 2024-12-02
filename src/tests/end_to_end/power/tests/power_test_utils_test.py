#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for the perf metric publishing code."""

import dataclasses
import math
import signal
import tempfile
import time
import unittest
import unittest.mock as mock
from pathlib import Path

from power_test_utils import power_test_utils
from trace_processing import trace_model, trace_time

_METRIC_NAME = "M3tr1cN4m3"
_MEASUREPOWER_PATH = "path/to/power"


class PowerSamplerTest(unittest.TestCase):
    """Tests for PowerSampler"""

    def setUp(self) -> None:
        self.output_dir = tempfile.TemporaryDirectory()
        self.output_dir_path = Path(self.output_dir.name)
        self.expected_csv_output_path = Path(
            self.output_dir_path, f"{_METRIC_NAME}_power_samples.csv"
        )
        self.assertFalse(self.expected_csv_output_path.exists())

        self.default_config = power_test_utils.PowerSamplerConfig(
            output_dir=str(self.output_dir_path),
            metric_name=_METRIC_NAME,
            measurepower_path=None,
        )

        self.config_width_measurepower_path = dataclasses.replace(
            self.default_config, measurepower_path=_MEASUREPOWER_PATH
        )

    def test_sampler_without_measurepower(self) -> None:
        """Tests that PowerSampler creates zero results when not given a path to a measurepower binary."""
        with mock.patch("os.environ.get", return_value=None):
            sampler = power_test_utils.create_power_sampler(self.default_config)

        with mock.patch.object(time, "time", return_value=5):
            sampler.start()

        with mock.patch.object(time, "time", return_value=10):
            sampler.stop()

        self.assertFalse(sampler.has_samples())
        self.assertFalse(sampler.extract_samples())

    def test_sampler_with_measurepower(self) -> None:
        """Tests PowerSampler when given a path to a measurepower binary.

        The sampler should interact with the binary via subprocess.Popen
        and an intermediate csv file.
        """
        sampler = power_test_utils.create_power_sampler(
            self.config_width_measurepower_path, fallback_to_stub=False
        )

        with mock.patch("subprocess.Popen") as mock_popen:
            mock_proc = mock_popen.return_value
            mock_proc.poll.return_value = None

            # Fake output from mock_proc
            self.expected_csv_output_path.write_text(
                """Mandatory Timestamp,Current,Voltage
0,0,12
1000000000,25,12
2000000000,100,12
4000000000,75,12
""",
                encoding="utf-8",
            )

            sampler.start()

            mock_popen.assert_called()
            self.assertEqual(
                mock_popen.call_args.args[0],
                [
                    _MEASUREPOWER_PATH,
                    "-format",
                    "csv",
                    "-out",
                    str(self.expected_csv_output_path),
                ],
            )

            mock_proc.wait.return_value = 0
            sampler.stop()

            mock_proc.send_signal.assert_called_with(signal.SIGINT)
        self.assertEqual(
            sampler.extract_samples(),
            [
                power_test_utils.Sample("0", "0", "12", None),
                power_test_utils.Sample("1000000000", "25", "12", None),
                power_test_utils.Sample("2000000000", "100", "12", None),
                power_test_utils.Sample("4000000000", "75", "12", None),
            ],
        )

    @mock.patch("subprocess.Popen")
    @mock.patch("time.time")
    @mock.patch("time.sleep")
    def test_sampler_with_measurepower_timeout(
        self,
        mock_sleep: mock.MagicMock,
        mock_time: mock.MagicMock,
        mock_popen: mock.MagicMock,
    ) -> None:
        """Tests the sampler with a measurepower binary path that times out"""
        sampler = power_test_utils.create_power_sampler(
            self.config_width_measurepower_path, fallback_to_stub=False
        )

        mock_proc = mock_popen.return_value
        mock_proc.poll.return_value = None

        self.assertFalse(self.expected_csv_output_path.exists())

        # # Fake current time:
        mock_time.side_effect = [0, 30, 61]
        with self.assertRaises(TimeoutError):
            sampler.start()

        mock_sleep.assert_called_with(1)

    def test_create_power_sampler_without_measurepower(self) -> None:
        with mock.patch("os.environ.get", return_value=None):
            sampler = power_test_utils.create_power_sampler(self.default_config)
            self.assertIsInstance(sampler, power_test_utils._NoopPowerSampler)

    def test_create_power_sampler_without_measurepower_without_fallback_to_stub(
        self,
    ) -> None:
        with mock.patch("os.environ.get", return_value=None):
            with self.assertRaisesRegex(
                RuntimeError, ".* env variable must be set"
            ):
                power_test_utils.create_power_sampler(
                    self.default_config, fallback_to_stub=False
                )

    def test_create_power_sampler_with_measurepower_env_var(self) -> None:
        with mock.patch("os.environ.get", return_value="path/to/power"):
            sampler = power_test_utils.create_power_sampler(
                self.config_width_measurepower_path
            )
            self.assertIsInstance(sampler, power_test_utils._RealPowerSampler)

    def test_weighted_average(self) -> None:
        vals = [3, 2, 3, 4]
        weights = [1, 1, 1, 1]
        self.assertEqual(power_test_utils.weighted_average(vals, weights), 3)

        vals = [3, 2, 3, 4]
        weights = [1, 2, 3, 4]
        # (3 + 4 + 9 + 16) / 10 = 3.2
        self.assertEqual(power_test_utils.weighted_average(vals, weights), 3.2)

    def test_cross_correlate_arg_max(self) -> None:
        signal = [1, 2, 3, 4, 5, 6]
        feature = [1]
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(signal, feature), (6, 5)
        )

        signal = [1, 2, 3, 4, 5, 6]
        feature = [1, 2]
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(signal, feature), (17, 4)
        )

        signal = [0, 0, 0, 1, 4, 3, 2, 0]
        feature = [1, 4, 3, 2]
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(signal, feature), (30, 3)
        )

        large_signal = list(range(30000))
        large_feature = list(range(20000))
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(
                large_signal, large_feature
            ),
            (4666366670000, 10000),
        )

    def test_normalize(self) -> None:
        signal = [0, 1, 2, 3, 4, 5]
        normalized = power_test_utils.normalize(signal)
        expected = [0, 0.2, 0.4, 0.6, 0.8, 1.0]
        self.assertEqual(normalized, expected)

    def test_normalized_xcorrelate(self) -> None:
        signal = [5, 0, 0, 0, 0, 2, 2, 2, 2, 2]
        feature = [10, 5, 5, 5, 5]

        # If we cross correlate without normalizing, we correlate best with the 2s
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(signal, feature), (60, 5)
        )

        # But if we normalize first
        # We'll have
        #
        # signal: [1, 0, 0, 0, 0, .4, .4, .4, .4]
        # feature: [1, 0, 0, 0, 0]
        #
        # which correctly correlates best with the beginning
        self.assertEqual(
            power_test_utils.cross_correlate_arg_max(
                power_test_utils.normalize(signal),
                power_test_utils.normalize(feature),
            ),
            (1.0, 0),
        )


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
        sample = power_test_utils.GonkSample.from_values(
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
        sample = power_test_utils.GonkSample.from_values(
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
    def _create_gpio_gonk_sample(
        cls, nanoseconds: int
    ) -> power_test_utils.GonkSample:
        n_rails = len(cls.RAIL_NAMES)
        return power_test_utils.GonkSample(
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
        power_test_utils.merge_gonk_data(
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
        power_test_utils.merge_gonk_data(
            model, gonk_samples, trace_file.name, self.RAIL_NAMES
        )
