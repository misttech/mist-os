#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for the perf metric publishing code."""
# keep-sorted start

import dataclasses
import signal
import tempfile
import time
import unittest
import unittest.mock as mock

# keep-sorted end

# keep-sorted start
from pathlib import Path
from power_test_utils import power_test_utils
from trace_processing import trace_metrics

# keep-sorted end

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

        self.assertEqual(sampler.to_fuchsiaperf_results(), [])

    @mock.patch("subprocess.Popen")
    def test_sampler_with_measurepower(
        self, mock_popen: mock.MagicMock
    ) -> None:
        """Tests PowerSampler when given a path to a measurepower binary.

        The sampler should interact with the binary via subprocess.Popen
        and an intermediate csv file.
        """
        sampler = power_test_utils.create_power_sampler(
            self.config_width_measurepower_path, fallback_to_stub=False
        )

        mock_proc = mock_popen.return_value
        mock_proc.poll.return_value = None

        # Fake output from mock_proc
        self.expected_csv_output_path.write_text(
            """Timestamp,Current,Voltage
0,0,12
1000000000,25,12
2000000000,100,12
4000000000,75,12
"""
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

        results = sampler.to_fuchsiaperf_results()
        self.assertEqual(
            results,
            [
                trace_metrics.TestCaseResult(
                    label="MinPower",
                    unit=trace_metrics.Unit.watts,
                    values=[0.0],
                ),
                trace_metrics.TestCaseResult(
                    label="MeanPower",
                    unit=trace_metrics.Unit.watts,
                    values=[0.6],
                ),
                trace_metrics.TestCaseResult(
                    label="MaxPower",
                    unit=trace_metrics.Unit.watts,
                    values=[1.2],
                ),
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
