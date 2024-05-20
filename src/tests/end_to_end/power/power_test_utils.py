#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power test utility library."""

# keep-sorted start
import abc
import csv
import dataclasses
import enum
import itertools
import logging
import operator
import os
import pathlib
import signal
import struct
import subprocess
import time

# keep-sorted end

# keep-sorted start
from collections import deque
from collections.abc import Iterable, Mapping
from trace_processing import trace_metrics, trace_model, trace_time
from typing import Sequence

# keep-sorted end


SAMPLE_INTERVAL_NS = 200000

# Traces use "ticks" which is a hardware dependent time duration
TICKS_PER_NS = 0.024

_LOGGER = logging.getLogger(__name__)

# The measurepower tool's path. The tool is expected to periodically output
# power measurements into a csv file, with _PowerCsvHeaders.all() columns.
_MEASUREPOWER_PATH_ENV_VARIABLE = "MEASUREPOWER_PATH"


def _avg(avg: float, value: float, count: int) -> float:
    return avg + (value - avg) / count


def weighted_average(arr: Iterable[float], weights: Iterable[int]) -> float:
    return sum(
        itertools.starmap(
            operator.mul,
            zip(arr, weights, strict=True),
        )
    ) / sum(weights)


# We don't have numpy in the vendored python libraries so we'll have to roll our own correlate
# functionality which will be slooooow.
#
# Normally, we'd compute the cross correlation and then take the argmax to line up the signals the
# closest. Instead we'll do the argmax and the correlation at the same time to save on allocations.
def cross_correlate_arg_max(
    signal: Sequence[float], feature: Sequence[float]
) -> tuple[float, int]:
    """Cross correlate two 1d signals and return the maximum correlation. Correlation is only done
    where the signals overlap completely.

    Returns
        (correlation, idx)
        Where correlation is the maximum correlation between the signals and idx is the argmax that
        it occurred at.
    """
    # Slide our feature across the signal and compute the dot product at each point
    return max(
        # Produce items of the form [(correlation1, idx1), (correlation2, idx2), ...] of which we
        # want the highest correlation.
        map(
            lambda base: (
                # Fancy itertools based dot product
                sum(
                    itertools.starmap(
                        operator.mul,
                        zip(
                            itertools.islice(signal, base, base + len(feature)),
                            feature,
                            strict=True,
                        ),
                    )
                ),
                base,
            ),
            # Take a sliding window dot product. E.g. if we have the arrays
            # [1,2,3,4] and [1,2]
            #
            # The sliding window dot product is
            # [
            #     [1,2] o [1,2],
            #     [2,3] o [1,2],
            #     [3,5] o [1,2],
            # ]
            range(len(signal) - len(feature) + 1),
        )
    )


@dataclasses.dataclass
class PowerMetricSample:
    """A sample of collected power metrics.

    Args:
      timestamp: timestamp of sample in nanoseconds since epoch.
      voltage: voltage in Volts.
      current: current in milliAmpere.
      raw_aux: (optional) The raw 16 bit fine aux channel reading from a Monsoon power monitor.  Can
               optionally be used for log synchronization and alignment
    """

    timestamp: int
    voltage: float
    current: float
    raw_aux: int | None

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

    def process_sample(self, sample: PowerMetricSample) -> None:
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
#
# One of two formats for the CSV data is expected.  If the first column is named
# simply "Timestamp", where the scripts are expected to synthesize nominal
# timestamps for the samples.  If it is named "Mandatory Timestamp" instead,
# then it is data captured from a script which has already synthesized its own
# timestamps, accounting for dropped samples, calibration samples, and the power
# monitor deviations from nominal.  These timestamps should be simply taken and
# used as is.
#
# In the second case, the script can (if asked to) also capture a 4th column of
# data representing the raw 16 bit readings from the fine auxiliary current
# channel.  When present, these readings provide an alternative method for
# aligning the timelines of the power monitor data with the trace data.
#
class _PowerCsvHeaders(enum.StrEnum):
    MANDATORY_TIMESTAMP = "Mandatory Timestamp"
    TIMESTAMP = "Timestamp"
    CURRENT = "Current"
    VOLTAGE = "Voltage"
    AUX_CURRENT = "Raw Aux"

    @staticmethod
    def assert_header(header: list[str]) -> None:
        assert header[1] == _PowerCsvHeaders.CURRENT
        assert header[2] == _PowerCsvHeaders.VOLTAGE
        if header[0] == _PowerCsvHeaders.MANDATORY_TIMESTAMP:
            assert len(header) == 3 or header[3] == _PowerCsvHeaders.AUX_CURRENT
        else:
            assert header[0] == _PowerCsvHeaders.TIMESTAMP


# TODO(b/320778225): Make this class private and stateless by changing all callers to use
# MetricsSampler directly.
class PowerMetricsProcessor(trace_metrics.MetricsProcessor):
    """Power metric processor to extract performance data from raw samples.

    Args:
      power_samples_path: path to power samples CSV file.
    """

    def __init__(self, power_samples_path: str) -> None:
        self._power_samples_path: str = power_samples_path
        self._power_metrics: AggregatePowerMetrics = AggregatePowerMetrics()

    # Implements MetricsProcessor.process_metrics. Model is unused and is None
    # to support legacy users.
    def process_metrics(
        self, model: trace_model.Model | None = None
    ) -> list[trace_metrics.TestCaseResult]:
        """Coverts CSV samples into aggregate metrics."""
        with open(self._power_samples_path, "r") as f:
            reader = csv.reader(f)
            header = next(reader)
            _PowerCsvHeaders.assert_header(header)
            for row in reader:
                sample = PowerMetricSample(
                    timestamp=int(row[0]),
                    voltage=float(row[1]),
                    current=float(row[2]),
                    raw_aux=int(row[3]) if len(row) >= 4 else None,
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
        trace_results: list[trace_metrics.TestCaseResult] | None = None,
    ) -> str:
        """Writes the fuchsia_perf JSON file to specified directory.

        Args:
          output_dir: path to the output directory to write to.
          metric_name: name of the power metric being measured.
          trace_metrics: trace-based metrics to include in the output.

        Returns:
          The fuchiaperf.json file generated by trace processing.
        """
        results = self.to_fuchsiaperf_results() + (trace_results or [])

        fuchsiaperf_json_path = os.path.join(
            output_dir,
            f"{metric_name}_power.fuchsiaperf.json",
        )

        trace_metrics.TestCaseResult.write_fuchsiaperf_json(
            results=results,
            test_suite=f"fuchsia.power.{metric_name}",
            output_path=pathlib.Path(fuchsiaperf_json_path),
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

    def should_generate_load(self) -> bool:
        return False

    def merge_power_data(self, model: trace_model.Model, fxt_path: str) -> None:
        pass

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

    def __init__(self, config: PowerSamplerConfig) -> None:
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
        self._measurepower_proc: subprocess.Popen[str] | None = None
        self._csv_output_path = os.path.join(
            self._config.output_dir,
            f"{self._config.metric_name}_power_samples.csv",
        )

    def _start_impl(self) -> None:
        _LOGGER.info("Starting power sampling")
        self._start_power_measurement()
        self._await_first_sample()

    def _stop_impl(self) -> None:
        _LOGGER.info("Stopping power sampling...")
        self._stop_power_measurement()
        _LOGGER.info("Power sampling stopped")

    def _metrics_processor_impl(self) -> trace_metrics.MetricsProcessor:
        return PowerMetricsProcessor(power_samples_path=self._csv_output_path)

    def _start_power_measurement(self) -> None:
        assert self._config.measurepower_path
        cmd = [
            self._config.measurepower_path,
            "-format",
            "csv",
            "-out",
            self._csv_output_path,
        ]
        _LOGGER.debug(f"Power measurement cmd: {cmd}")
        self._measurepower_proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )

    def _await_first_sample(self, timeout_sec: float = 60) -> None:
        _LOGGER.debug(f"Awaiting 1st power sample (timeout_sec={timeout_sec})")
        assert self._measurepower_proc
        proc = self._measurepower_proc
        csv_path = self._csv_output_path
        deadline = time.time() + timeout_sec

        while time.time() < deadline:
            if proc.poll():
                stdout = proc.stdout.read() if proc.stdout else None
                stderr = proc.stderr.read() if proc.stderr else None
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

    def _stop_power_measurement(self, timeout_sec: float = 60) -> None:
        _LOGGER.debug("Stopping the measurepower process...")
        proc = self._measurepower_proc
        assert proc
        proc.send_signal(signal.SIGINT)
        result = proc.wait(timeout_sec)
        if result:
            stdout = proc.stdout.read() if proc.stdout else None
            stderr = proc.stderr.read() if proc.stderr else None
            raise RuntimeError(
                f"Measure power failed once stopped with status"
                f"{proc.returncode} stdout: {stdout} "
                f"stderr: {stderr}"
            )
        _LOGGER.debug("measurepower process stopped.")

    def should_generate_load(self) -> bool:
        return True

    def merge_power_data(self, model: trace_model.Model, fxt_path: str) -> None:
        merge_power_data(model, self._csv_output_path, fxt_path)


def create_power_sampler(
    config: PowerSamplerConfig, fallback_to_stub: bool = True
) -> PowerSampler:
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


def read_fuchsia_trace_cpu_usage(
    model: trace_model.Model,
) -> Mapping[int, Sequence[tuple[trace_time.TimePoint, float]]]:
    """
    Read through the given fuchsia trace and return:

    Args:
        model: the model to extrace cpu usage data from

    Returns:
        {cpu: [timestamp, usage]}
        where the timestamp is in ticks and usage is a 0.0 or 1.0 depending on if a
        processes was scheduled for that interval.
    """
    scheduling_intervals = {}

    for cpu, intervals in model.scheduling_records.items():
        scheduling_intervals[cpu] = list(
            map(
                lambda record: (record.start, 0.0 if record.is_idle() else 1.0),
                # Drop the waking records, the don't count towards cpu usage.
                filter(
                    lambda record: isinstance(
                        record, trace_model.ContextSwitch
                    ),
                    intervals,
                ),
            )
        )
        # The records are _probably_ sorted as is, but there's no guarantee of it.
        # Let's guarantee it.
        scheduling_intervals[cpu].sort(key=lambda kv: kv[0])

    return scheduling_intervals


class Sample:
    def __init__(
        self,
        timestamp: str,
        current: str,
        voltage: str,
        aux_current: str | None,
    ) -> None:
        self.timestamp = int(timestamp)
        self.current = float(current)
        self.voltage = float(voltage)
        self.aux_current = (
            float(aux_current) if aux_current is not None else None
        )


def read_power_samples(power_trace_path: str) -> list[Sample]:
    """Return a tuple of the current and power samples from the power csv"""
    samples: list[Sample] = []
    with open(power_trace_path, "r") as power_csv:
        reader = csv.reader(power_csv)
        header = next(reader)
        sample_count = 0

        if header[0] == "Mandatory Timestamp":
            if len(header) < 4:
                create_sample = lambda line, count: Sample(
                    line[0], line[1], line[2], None
                )
            else:
                create_sample = lambda line, count: Sample(
                    line[0], line[1], line[2], line[3]
                )
        else:
            create_sample = lambda line, count: Sample(
                count * SAMPLE_INTERVAL_NS, line[1], line[2], None
            )

        for line in reader:
            samples.append(create_sample(line, sample_count))
            sample_count += 1

    return samples


def append_power_data(
    fxt_path: str,
    power_samples: list[Sample],
    starting_ticks: int,
) -> None:
    """
    Given a list of voltage and current samples spaced 200us apart and a starting timestamp, append
    them to the trace at `fxt_path`.

    Args:
        fxt_path: the fxt file to write to
        power_samples: the samples to append
        starting_ticks: offset from the beginning of the trace in "ticks"
    """
    with open(fxt_path, "ab") as merged_trace:
        # Virtual koids have a top bit as 1, the remaining doesn't matter as long as it's unique.
        fake_process_koid = 0x8C01_1EC7_EDDA_7A10  # CollectedData10
        fake_thread_koid = 0x8C01_1EC7_EDDA_7A20  # CollectedData20
        fake_thread_ref = 0xFF

        def inline_string_ref(string: str) -> int:
            # Inline fxt ids have their top bit set to 1. The remaining bits indicate the number of
            # inline bytes.
            return 0x8000 | len(string)

        # See //docs/reference/tracing/trace-format for the below trace format
        def thread_record_header(thread_ref: int) -> int:
            thread_record_type = 3
            thread_record_size_words = 3
            return (
                thread_ref << 16
                | thread_record_size_words << 4
                | thread_record_type
            )

        def kernel_object_record_header(
            num_args: int, name_ref: int, obj_type: int, size_words: int
        ) -> int:
            kernel_object_record_header_type = 7
            return (
                num_args << 40
                | name_ref << 24
                | obj_type << 16
                | size_words << 4
                | kernel_object_record_header_type
            )

        # The a fake process and thread records
        merged_trace.write(
            thread_record_header(fake_thread_ref).to_bytes(8, "little")
        )
        merged_trace.write(fake_process_koid.to_bytes(8, "little"))
        merged_trace.write(fake_thread_koid.to_bytes(8, "little"))

        ZX_OBJ_TYPE_PROCESS = 1
        ZX_OBJ_TYPE_THREAD = 2

        # Name the fake process
        merged_trace.write(
            kernel_object_record_header(
                0,
                inline_string_ref("Power Measurements"),
                ZX_OBJ_TYPE_PROCESS,
                5,  # 1 word header, 1 word koid, 3 words for name stream
            ).to_bytes(8, "little")
        )
        merged_trace.write(fake_process_koid.to_bytes(8, "little"))
        merged_trace.write(b"Power Measurements\0\0\0\0\0\0")

        # Name the fake thread
        merged_trace.write(
            kernel_object_record_header(
                0,
                inline_string_ref("Power Measurements"),
                ZX_OBJ_TYPE_THREAD,
                5,  # 1 word header, 1 word koid, 3 words for name stream
            ).to_bytes(8, "little")
        )
        merged_trace.write(fake_thread_koid.to_bytes(8, "little"))
        merged_trace.write(b"Power Measurements\0\0\0\0\0\0")

        def counter_event_header(
            name_id: int,
            category_id: int,
            thread_ref: int,
            num_args: int,
            record_words: int,
        ) -> int:
            counter_event_type = 1 << 16
            event_record_type = 4
            return (
                (name_id << 48)
                | (category_id << 32)
                | (thread_ref << 24)
                | (num_args << 20)
                | counter_event_type
                | record_words << 4
                | event_record_type
            )

        # We write all our data to the same counter
        COUNTER_ID = 0x000_0000_0000_0001

        # Now write our sample data as counter events into the trace
        for sample in power_samples:
            # We will be providing either 3 or 4 arguments, depending on whether
            # or not this sample has raw aux current data in it.  Each argument
            # takes 3 words of storage.
            arg_count = 4 if sample.aux_current is not None else 3
            arg_words = arg_count * 3

            # Emit the counter track
            merged_trace.write(
                counter_event_header(
                    inline_string_ref("Metrics"),
                    inline_string_ref("Metrics"),
                    0xFF,
                    arg_count,
                    # 1 word counter, 1 word ts,
                    # 2 words inline strings
                    # |arg_words| words of arguments,
                    # 1 word counter id = 5 + |arg_words|
                    5 + arg_words,
                ).to_bytes(8, "little")
            )
            timestamp_ticks = int(
                (sample.timestamp * TICKS_PER_NS) + starting_ticks
            )
            merged_trace.write(timestamp_ticks.to_bytes(8, "little"))
            # Inline strings need to be 0 padded to a multiple of 8 bytes.
            merged_trace.write(b"Metrics\0")
            merged_trace.write(b"Metrics\0")

            def double_argument_header(name_ref: int, size: int) -> int:
                argument_type = 5
                return name_ref << 16 | size << 4 | argument_type

            # Write the Voltage
            merged_trace.write(
                double_argument_header(
                    inline_string_ref("Voltage"), 3
                ).to_bytes(8, "little")
            )
            merged_trace.write(b"Voltage\0")
            data = [sample.voltage]
            s = struct.pack("d" * len(data), *data)
            merged_trace.write(s)

            # Write the Current
            merged_trace.write(
                double_argument_header(
                    inline_string_ref("Current"), 3
                ).to_bytes(8, "little")
            )
            merged_trace.write(b"Current\0")
            data = [sample.current]
            s = struct.pack("d" * len(data), *data)
            merged_trace.write(s)

            # Write the Power
            merged_trace.write(
                double_argument_header(inline_string_ref("Power"), 3).to_bytes(
                    8, "little"
                )
            )
            merged_trace.write(b"Power\0\0\0")
            data = [sample.current * sample.voltage]
            s = struct.pack("d" * len(data), *data)
            merged_trace.write(s)

            # Write the raw aux current, if present.
            if sample.aux_current is not None:
                merged_trace.write(
                    double_argument_header(
                        inline_string_ref("Raw Aux"), 3
                    ).to_bytes(8, "little")
                )
                merged_trace.write(b"Raw Aux\0")
                data = [sample.aux_current]
                s = struct.pack("d" * len(data), *data)
                merged_trace.write(s)

            # Write the counter_id
            merged_trace.write(COUNTER_ID.to_bytes(8, "little"))


def build_usage_samples(
    scheduling_intervals: Mapping[
        int, Sequence[tuple[trace_time.TimePoint, float]]
    ]
) -> dict[int, list[float]]:
    usage_samples = {}
    # Our per cpu records likely don't all start and end at the same time.
    # We'll pad the other cpu tracks with idle time on either side.
    max_len = 0
    earliest_ts = min(x[0][0] for x in scheduling_intervals.values())
    for cpu, intervals in scheduling_intervals.items():
        # The power samples are a fixed interval apart. We'll synthesize cpu
        # usage samples that also track over the same interval in this array.
        cpu_usage_samples = []

        # To make the conversion, let's start by converting our list of
        # [(start_time, work)] into [(duration, work)]. We could probably do this
        # in one pass, but doing this conversion first makes the logic easier
        # to reason about.
        (prev_ts, prev_work) = intervals[0]

        # Idle pad the beginning of our intervals to start from a fixed timestamp.
        weighted_work = deque([(prev_ts - earliest_ts, 0.0)])
        for ts, work in intervals[1:]:
            weighted_work.append((ts - prev_ts, prev_work))
            (prev_ts, prev_work) = (ts, work)

        # Finally, to get our fixed sample intervals, we'll use our [(duration,
        # work)] list and pop chunks of work `sample_interval` ticks at a time.
        # Then once we've accumulated enough weighted work to fill the
        # schedule, we'll take the weighted average and call that our cpu usage
        # for the interval.
        usage: list[float] = []
        durations: list[int] = []
        interval_duration_remaining = trace_time.TimeDelta(SAMPLE_INTERVAL_NS)

        for duration, work in weighted_work:
            if interval_duration_remaining - duration >= trace_time.TimeDelta():
                # This duration doesn't fill or finish a full sample interval,
                # just append it to the accumulator lists.
                usage.append(work)
                durations.append(duration.to_nanoseconds())
                interval_duration_remaining -= duration
            else:
                # We have enough work to record a sample. Insert what we need
                # to top off the interval and add it to cpu_usage_samples
                partial_duration = interval_duration_remaining
                usage.append(work)
                durations.append(partial_duration.to_nanoseconds())
                duration -= partial_duration

                average = weighted_average(usage, durations)
                cpu_usage_samples.append(average)

                # Now use up the rest of the duration. Not that it's possible
                # that this duration might actually be longer a full sampling
                # interval so we should synthesize multiple samples in that
                # case.
                remaining_duration = trace_time.TimeDelta(
                    duration.to_nanoseconds() % SAMPLE_INTERVAL_NS
                )
                num_extra_intervals = int(
                    duration.to_nanoseconds() / SAMPLE_INTERVAL_NS
                )
                cpu_usage_samples.extend(
                    [work for _ in range(0, num_extra_intervals)]
                )

                # Reset our accumulators with the leftover bits
                usage = [work]
                durations = [remaining_duration.to_nanoseconds()]
                interval_duration_remaining = (
                    trace_time.TimeDelta(SAMPLE_INTERVAL_NS)
                    - remaining_duration
                )

        usage_samples[cpu] = cpu_usage_samples
        max_len = max(max_len, len(cpu_usage_samples))

    # Idle pad the end of our intervals to all contain the same number
    for cpu, samples in usage_samples.items():
        samples.extend([0 for _ in range(len(samples), max_len)])
    return usage_samples


def merge_power_data(
    model: trace_model.Model, power_trace_path: str, fxt_path: str
) -> None:
    # We'll start by reading in the fuchsia cpu data from the trace model
    scheduling_intervals = read_fuchsia_trace_cpu_usage(model)
    power_samples = read_power_samples(power_trace_path)

    # We can't just append the power data to the beginning of the trace. The
    # trace starts before the power collection starts at some unknown offset.
    # We'll have to first find this offset.
    #
    # We know that CPU usage and current draw are highly correlated. So we'll
    # have the test start by running a cpu intensive workload in a known
    # pattern for the first few seconds before starting the test. We can then
    # synthesize cpu usage over the same duration and attempt to correlate the
    # signals.

    # The power samples come in at fixed intervals. We'll construct cpu usage
    # data in the same intervals to attempt to correlate it at each offset.
    # We'll assume the highest correlated offset is the delay the power
    # sampling started at and we'll insert the samples starting at that
    # timepoint.
    earliest_ts = min(x[0][0] for x in scheduling_intervals.values())

    # The trace model give us per cpu information about which processes start at
    # which time. To compare it to the power samples we need to convert it into
    # something of the form ["usage_sample_1", "usage_sample_2", ...] where
    # each "usage_sample_n" is the cpu usage over a 200us duration.
    usage_samples = build_usage_samples(scheduling_intervals)

    # Now take take the average cpu usage across each cpu to get overall cpu usage which should
    # correlate to our power/current samples.
    merged = [samples for _, samples in usage_samples.items()]
    avg_cpu_combined = [0.0] * len(merged[0])

    for i, _ in enumerate(avg_cpu_combined):
        total: float = 0
        for sample_list in merged:
            total += sample_list[i]
        avg_cpu_combined[i] = total / len(merged)

    # Finally, we can get the cross correlation between power and cpu usage. We run a known cpu
    # heavy workload in the first 5ish seconds of the test so we limit our signal correlation to
    # that portion. Power and CPU readings can be offset in either direction, but shouldn't be
    # separated by more than a second. Thus, we take the first 4 seconds of the power readings,
    # attempt to match them up with the first 5 seconds of CPU readings, and then vice-versa.
    # Afterwards, we choose an alignment by picking the higher of the two correlation scores.
    (
        power_after_cpu_correlation,
        power_after_cpu_correlation_idx,
    ) = cross_correlate_arg_max(
        avg_cpu_combined[0:25000], [s.current for s in power_samples[0:20000]]
    )
    (
        cpu_after_power_correlation,
        cpu_after_power_correlation_idx,
    ) = cross_correlate_arg_max(
        [s.current for s in power_samples[0:25000]], avg_cpu_combined[0:20000]
    )
    starting_ticks = 0
    if power_after_cpu_correlation >= cpu_after_power_correlation:
        offset_ns = power_samples[power_after_cpu_correlation_idx].timestamp

        print(f"Delaying power readings by {offset_ns/1000/1000}ms")
        starting_ticks = int(
            (earliest_ts + trace_time.TimeDelta(offset_ns))
            .to_epoch_delta()
            .to_nanoseconds()
            * TICKS_PER_NS
        )
    else:
        offset_ns = power_samples[cpu_after_power_correlation_idx].timestamp

        print(f"Delaying CPU trace by {offset_ns/1000/1000}ms")
        starting_ticks = int(
            (earliest_ts - trace_time.TimeDelta(offset_ns))
            .to_epoch_delta()
            .to_nanoseconds()
            * TICKS_PER_NS
        )

    print(f"Aligning Power Trace to start at {starting_ticks} ticks")
    append_power_data(fxt_path, power_samples, starting_ticks)
