#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power test utility library."""

import abc
import csv
import dataclasses
import enum
import logging
import os
import signal
import subprocess
import time

from trace_processing import trace_metrics, trace_model

_LOGGER = logging.getLogger(__name__)

# The measurepower tool's path. The tool is expected to periodically output
# power measurements into a csv file, with _PowerCsvHeaders.all() columns.
_MEASUREPOWER_PATH_ENV_VARIABLE = "MEASUREPOWER_PATH"


def _avg(avg: float, value: float, count: int) -> float:
    return avg + (value - avg) / count


@dataclasses.dataclass
class PowerMetricSample:
    """A sample of collected power metrics.

    Args:
      timestamp: timestamp of sample in nanoseconds since epoch.
      voltage: voltage in Volts.
      current: current in milliAmpere.
    """

    timestamp: int
    voltage: float
    current: float

    def compute_power(self) -> float:
        """Compute the power in Watts from sample.

        Returns:
          Power in Watts.
        """
        return self.voltage * self.current * 1e-3


@dataclasses.dataclass
class AggregatePowerMetrics:
    """Aggregate power metrics representation.

    Represents aggregated metrics over a number of power metrics samples.

    Args:
      sample_count: number of power metric samples.
      max_power: maximum power in Watts over all samples.
      mean_power: average power in Watts over all samples.
      min_power: minimum power in Watts over all samples.
    """

    sample_count: int = 0
    max_power: float = float("-inf")
    mean_power: float = 0
    min_power: float = float("inf")

    def process_sample(self, sample: PowerMetricSample):
        """Process a sample of power metrics.

        Args:
            sample: A sample of power metrics.
        """
        power = sample.compute_power()
        self.sample_count += 1
        self.max_power = max(self.max_power, power)
        self.mean_power = _avg(self.mean_power, power, self.sample_count)
        self.min_power = min(self.min_power, power)

    def to_fuchsiaperf_results(self) -> list[trace_metrics.TestCaseResult]:
        """Converts Power metrics to fuchsiaperf JSON object.

        Returns:
          List of JSON object.
        """
        results: list[trace_metrics.TestCaseResult] = [
            trace_metrics.TestCaseResult(
                label="MinPower",
                unit=trace_metrics.Unit.watts,
                values=[self.min_power],
            ),
            trace_metrics.TestCaseResult(
                label="MeanPower",
                unit=trace_metrics.Unit.watts,
                values=[self.mean_power],
            ),
            trace_metrics.TestCaseResult(
                label="MaxPower",
                unit=trace_metrics.Unit.watts,
                values=[self.max_power],
            ),
        ]
        return results


# Constants class
class _PowerCsvHeaders(enum.StrEnum):
    TIMESTAMP = "Timestamp"
    CURRENT = "Current"
    VOLTAGE = "Voltage"

    def all():
        return list(_PowerCsvHeaders)


# TODO(b/320778225): Make this class private and stateless by changing all callers to use
# MetricsSampler directly.
class PowerMetricsProcessor(trace_metrics.MetricsProcessor):
    """Power metric processor to extract performance data from raw samples.

    Args:
      power_samples_path: path to power samples CSV file.
    """

    def __init__(self, power_samples_path: str):
        self._power_samples_path: str = power_samples_path
        self._power_metrics: AggregatePowerMetrics = AggregatePowerMetrics()

    # Implements MetricsProcessor.process_metrics. Model is unused and is None
    # to support legacy users.
    def process_metrics(
        self, model: trace_model.Model = None
    ) -> list[trace_metrics.TestCaseResult]:
        """Coverts CSV samples into aggregate metrics."""
        with open(self._power_samples_path, "r") as f:
            reader = csv.reader(f)
            header = next(reader)
            assert header[0:3] == _PowerCsvHeaders.all()
            for row in reader:
                sample = PowerMetricSample(
                    timestamp=int(row[0]),
                    voltage=float(row[1]),
                    current=float(row[2]),
                )
                self._power_metrics.process_sample(sample)
        return self._power_metrics.to_fuchsiaperf_results()

    # DEPRECATED: Use process_metrics return value.
    # TODO(b/320778225): Remove once downstream users are refactored.
    def to_fuchsiaperf_results(self) -> list[trace_metrics.TestCaseResult]:
        """Returns the processed TestCaseResults"""
        return self._power_metrics.to_fuchsiaperf_results()

    # DEPRECATED: Use process_metrics + trace_metrics.TestCaseResult.write_fuchsiaperf_json.
    # TODO(b/320778225): Remove once downstream users are refactored.
    def write_fuchsiaperf_json(
        self,
        output_dir: str,
        metric_name: str,
        trace_results: list[trace_metrics.TestCaseResult] = [],
    ) -> str:
        """Writes the fuchsia_perf JSON file to specified directory.

        Args:
          output_dir: path to the output directory to write to.
          metric_name: name of the power metric being measured.
          trace_metrics: trace-based metrics to include in the output.

        Returns:
          The fuchiaperf.json file generated by trace processing.
        """
        results = self.to_fuchsiaperf_results() + trace_results

        fuchsiaperf_json_path = os.path.join(
            output_dir,
            f"{metric_name}_power.fuchsiaperf.json",
        )

        trace_metrics.TestCaseResult.write_fuchsiaperf_json(
            results=results,
            test_suite=f"fuchsia.power.{metric_name}",
            output_path=fuchsiaperf_json_path,
        )

        return fuchsiaperf_json_path


@dataclasses.dataclass(frozen=True)
class PowerSamplerConfig:
    # Directory for samples output
    output_dir: str
    # Unique metric name, used in output file names.
    metric_name: str
    # Path of the measurepower tool (Optional)
    measurepower_path: str | None = None


class _PowerSamplerState(enum.Enum):
    INIT = 1
    STARTED = 2
    STOPPED = 3


class PowerSampler:
    """Power sampling base class.

    Usage:
    ```
    sampler:PowerSampler = create_power_sampler(...)
    sampler.start()
    ... interact with the device, also gather traces ...
    sampler.stop()

    sampler.metrics_processor().process_and_save(model, output_path="my_test.fuchsiaperf.json")
    ```

    Alternatively, the sampler can be combined with the results of other metric processors like this:
    ```
    power_sampler = PowerSampler(...)

    processor = MetricsProcessorSet([
      CpuMetricsProcessor(aggregates_only=True),
      FpsMetricsProcessor(aggregates_only=False),
      MyCustomProcessor(...),
      power_sampler.metrics_processor(),
    ])

    ... gather traces, start and stop the power sampler, create the model ...

    processor.process_and_save(model, output_path="my_test.fuchsiaperf.json")
    ```
    """

    def __init__(self, config: PowerSamplerConfig):
        """Creates a PowerSampler from a config.

        Args:
            config (PowerSamplerConfig): Configuration.
        """
        self._state: _PowerSamplerState = _PowerSamplerState.INIT
        self._config = config

    def start(self) -> None:
        """Starts sampling."""
        assert self._state == _PowerSamplerState.INIT
        self._state = _PowerSamplerState.STARTED
        self._start_impl()

    def stop(self) -> None:
        """Stops sampling. Has no effect if never started or already stopped."""
        if self._state == _PowerSamplerState.STARTED:
            self._state = _PowerSamplerState.STOPPED
            self._stop_impl()

    # DEPRECATED: Use .metric_processor().process_metrics() instead.
    # TODO(b/320778225): Remove once downstream users are refactored.
    def to_fuchsiaperf_results(self) -> list[trace_metrics.TestCaseResult]:
        """Returns power metrics TestCaseResults"""
        assert self._state == _PowerSamplerState.STOPPED
        return self.metrics_processor().process_metrics(
            model=trace_model.Model()
        )

    # Implements MetricsProcessor.process_metrics. Model is unused.

    def metrics_processor(self) -> trace_metrics.MetricsProcessor:
        """Returns a MetricsProcessor instance associated with the sampler."""
        return self._metrics_processor_impl()

    @abc.abstractmethod
    def _stop_impl(self) -> None:
        pass

    @abc.abstractmethod
    def _start_impl(self) -> None:
        pass

    @abc.abstractmethod
    def _metrics_processor_impl(self) -> trace_metrics.MetricsProcessor:
        pass


class _NoopPowerSampler(PowerSampler):
    """A no-op power sampler, used in environments where _MEASUREPOWER_PATH_ENV_VARIABLE isn't set."""

    def __init__(self, config: PowerSamplerConfig):
        """Constructor.

        Args:
            config (PowerSamplerConfig): Configuration.
        """
        super().__init__(config)

    def _start_impl(self) -> None:
        pass

    def _stop_impl(self) -> None:
        pass

    def _metrics_processor_impl(self) -> trace_metrics.MetricsProcessor:
        return trace_metrics.ConstantMetricsProcessor(results=[])


class _RealPowerSampler(PowerSampler):
    """Wrapper for the measurepower command-line tool."""

    def __init__(self, config: PowerSamplerConfig):
        """Constructor.

        Args:
            config (PowerSamplerConfig): Configuration.
        """
        super().__init__(config)
        assert config.measurepower_path
        self._measurepower_proc: subprocess.Popen | None = None
        self._csv_output_path = os.path.join(
            self._config.output_dir,
            f"{self._config.metric_name}_power_samples.csv",
        )

    def _start_impl(self):
        _LOGGER.info(f"Starting power sampling")
        self._start_power_measurement()
        self._await_first_sample()

    def _stop_impl(self):
        _LOGGER.info(f"Stopping power sampling...")
        self._stop_power_measurement()
        _LOGGER.info("Power sampling stopped")

    def _metrics_processor_impl(self) -> trace_metrics.MetricsProcessor:
        return PowerMetricsProcessor(power_samples_path=self._csv_output_path)

    def _start_power_measurement(self):
        cmd = [
            self._config.measurepower_path,
            "-format",
            "csv",
            "-out",
            self._csv_output_path,
        ]
        _LOGGER.debug(f"Power measurement cmd: {cmd}")
        self._measurepower_proc = subprocess.Popen(
            cmd=cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )

    def _await_first_sample(self, timeout_sec: float = 60):
        _LOGGER.debug(f"Awaiting 1st power sample (timeout_sec={timeout_sec})")
        proc = self._measurepower_proc
        csv_path = self._csv_output_path
        deadline = time.time() + timeout_sec

        while time.time() < deadline:
            if proc.poll():
                stdout = proc.stdout.read()
                stderr = proc.stderr.read()
                raise RuntimeError(
                    f"Measure power failed to start with status "
                    f"{proc.returncode} stdout: {stdout} "
                    f"stderr: {stderr}"
                )
            if os.path.exists(csv_path) and os.path.getsize(csv_path):
                _LOGGER.debug(f"Received 1st power sample in {csv_path}")
                return
            time.sleep(1)

        raise TimeoutError(
            f"Timed out after {timeout_sec} seconds while waiting for power samples"
        )

    def _stop_power_measurement(self, timeout_sec: float = 60):
        _LOGGER.debug("Stopping the measurepower process...")
        proc = self._measurepower_proc
        proc.send_signal(signal.SIGINT)
        result = proc.wait(timeout_sec)
        if result:
            stdout = proc.stdout.read()
            stderr = proc.stderr.read()
            raise RuntimeError(
                f"Measure power failed once stopped with status"
                f"{proc.returncode} stdout: {stdout} "
                f"stderr: {stderr}"
            )
        _LOGGER.debug("measurepower process stopped.")


def create_power_sampler(
    config: PowerSamplerConfig, fallback_to_stub: bool = True
):
    """Creates a power sampler.

    In the absence of `_MEASUREPOWER_PATH_ENV_VARIABLE`, creates a no-op sampler.
    """

    measurepower_path = config.measurepower_path or os.environ.get(
        _MEASUREPOWER_PATH_ENV_VARIABLE
    )
    if not measurepower_path:
        if not fallback_to_stub:
            raise RuntimeError(
                f"{_MEASUREPOWER_PATH_ENV_VARIABLE} env variable must be set"
            )

        _LOGGER.warning(
            f"{_MEASUREPOWER_PATH_ENV_VARIABLE} env variable not set. Using a no-op power sampler instead."
        )
        return _NoopPowerSampler(config)
    else:
        config = dataclasses.replace(
            config, measurepower_path=measurepower_path
        )
    return _RealPowerSampler(config)
