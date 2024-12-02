#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power test utility library."""

import abc
import csv
import dataclasses
import datetime
import enum
import itertools
import logging
import math
import operator
import os
import re
import signal
import struct
import subprocess
import time
from collections import deque
from collections.abc import Iterable, Iterator, Mapping
from typing import Any, BinaryIO, Self, Sequence

from trace_processing import trace_model, trace_time

SAMPLE_INTERVAL_NS = 200000

# Traces use "ticks" which is a hardware dependent time duration
TICKS_PER_NS = 0.024

_LOGGER = logging.getLogger(__name__)

# The measurepower tool's path. The tool is expected to periodically output
# power measurements into a csv file, with _PowerCsvHeaders.all() columns.
_MEASUREPOWER_PATH_ENV_VARIABLE = "MEASUREPOWER_PATH"


def weighted_average(arr: Iterable[float], weights: Iterable[int]) -> float:
    return sum(
        itertools.starmap(
            operator.mul,
            zip(arr, weights, strict=True),
        )
    ) / sum(weights)


def normalize(lst: Iterable[float]) -> list[float]:
    """Return a copy of the list normalized between [0, 1.0]."""
    lst_min = min(lst)
    lst_max = max(lst)
    return [(f - lst_min) / (lst_max - lst_min) for f in lst]


# We don't have numpy in the vendored python libraries so we'll have to roll our own correlate
# functionality which will be slooooow.
#
# Normally, we'd compute the cross correlation and then take the argmax to line up the signals the
# closest. Instead we'll do the argmax and the correlation at the same time to save on allocations.
#
# Precondition: len(signal) must be >= len(feature) for accurate results
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

    assert len(signal) >= len(feature)

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
            #     [3,4] o [1,2],
            # ]
            range(len(signal) - len(feature) + 1),
        )
    )


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

    def should_generate_load(self) -> bool:
        return False

    def has_samples(self) -> bool:
        return False

    def extract_samples(self) -> Sequence["Sample"]:
        """Return recorded samples. May be expensive and require I/O."""
        return []

    @abc.abstractmethod
    def _stop_impl(self) -> None:
        pass

    @abc.abstractmethod
    def _start_impl(self) -> None:
        pass


class _NoopPowerSampler(PowerSampler):
    """A no-op power sampler, used in environments where _MEASUREPOWER_PATH_ENV_VARIABLE isn't set."""

    def _start_impl(self) -> None:
        pass

    def _stop_impl(self) -> None:
        pass


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
        self._sampled_data = False

    def _start_impl(self) -> None:
        _LOGGER.info("Starting power sampling")
        self._start_power_measurement()
        self._await_first_sample()
        self._sampled_data = True

    def _stop_impl(self) -> None:
        _LOGGER.info("Stopping power sampling...")
        self._stop_power_measurement()
        _LOGGER.info("Power sampling stopped")

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

    def has_samples(self) -> bool:
        return self._sampled_data

    def extract_samples(self) -> Sequence["Sample"]:
        return read_power_samples(self._csv_output_path)


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
            f"{_MEASUREPOWER_PATH_ENV_VARIABLE} env variable not set. "
            "Using a no-op power sampler instead."
        )
        return _NoopPowerSampler(config)
    else:
        config = dataclasses.replace(
            config, measurepower_path=measurepower_path
        )
    return _RealPowerSampler(config)


def _read_fuchsia_trace_cpu_usage(
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
        timestamp: int | str,
        current: float | str,
        voltage: float | str,
        aux_current: float | str | None = None,
        power: float | str | None = None,
        rail_id: int = 1,
        rail_name: str | None = None,
    ) -> None:
        self.timestamp = int(timestamp)
        self.current = float(current)
        self.voltage = float(voltage)
        self.aux_current = (
            float(aux_current) if aux_current is not None else None
        )
        self.power = (
            float(power) if power is not None else self.current * self.voltage
        )
        self.rail_id = rail_id
        self.rail_name = rail_name

    def __eq__(self, other: Any) -> bool:
        if not isinstance(other, Sample):
            raise NotImplementedError()
        return (
            self.timestamp == other.timestamp
            and self.current == other.current
            and self.voltage == other.voltage
            and self.aux_current == other.aux_current
            and self.power == other.power
            and self.rail_id == other.rail_id
            and self.rail_name == other.rail_name
        )

    def __repr__(self) -> str:
        return (
            f"ts: {self.timestamp}ms, current: {self.current}, "
            f"voltage: {self.voltage}, aux: {self.aux_current}, "
            f"power: {self.power}, rail_id: {self.rail_id}, rail_name: {self.rail_name}"
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


def _append_power_data(
    fxt_path: str,
    power_samples: Sequence[Sample],
    starting_ticks: int,
) -> None:
    """
    Append a list of power samples to the trace at `fxt_path`.

    Args:
        fxt_path: the fxt file to write to
        power_samples: the samples to append
        starting_ticks: offset from the beginning of the trace in "ticks"
    """
    print(f"Aligning Power Trace to start at {starting_ticks} ticks")
    with open(fxt_path, "ab") as merged_trace:
        # Virtual koids have a top bit as 1, the remaining doesn't matter as long as it's unique.
        fake_process_koid = 0x8C01_1EC7_EDDA_7A10  # CollectedData10
        fake_thread_koid = 0x8C01_1EC7_EDDA_7A20  # CollectedData20
        fake_thread_ref = 0xFF

        BYTES_PER_WORD = 8

        class InlineString:
            def __init__(self, s: str):
                self.num_words: int = math.ceil(len(s) / BYTES_PER_WORD)

                # Inline fxt ids have their top bit set to 1.
                # The remaining bits indicate the number of inline bytes are valid.
                self.ref: int = 0x8000 | len(s)

                # Pad the word with 0x00 bytes to fill a full multiple of words.
                size = self.num_words * BYTES_PER_WORD
                assert (
                    len(s) <= size
                ), f"String {s} is longer than {size} bytes."
                tail_zeros = "\0" * (size - len(s))
                b = bytes(s + tail_zeros, "utf-8")
                assert (
                    len(b) == size
                ), f"Binary string {b!r} must be {size} bytes."
                self.data: bytes = b

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
        process_name = InlineString("Power Measurements")
        merged_trace.write(
            kernel_object_record_header(
                0,
                process_name.ref,
                ZX_OBJ_TYPE_PROCESS,
                process_name.num_words + 2,  # 1 word header, 1 word koid
            ).to_bytes(8, "little")
        )
        merged_trace.write(fake_process_koid.to_bytes(8, "little"))
        merged_trace.write(process_name.data)

        # Name the fake thread
        thread_name = InlineString("Power Measurements")
        merged_trace.write(
            kernel_object_record_header(
                0,
                thread_name.ref,
                ZX_OBJ_TYPE_THREAD,
                thread_name.num_words + 2,  # 1 word header, 1 word koid
            ).to_bytes(8, "little")
        )
        merged_trace.write(fake_thread_koid.to_bytes(8, "little"))
        merged_trace.write(thread_name.data)

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

        # Now write our sample data as counter events into the trace
        for sample in power_samples:
            category = InlineString("Metrics")
            name = InlineString(sample.rail_name or "Metrics")

            # We will be providing either 3 or 4 arguments, depending on whether
            # or not this sample has raw aux current data in it.
            arg_count = 4 if sample.aux_current is not None else 3
            words_per_arg = (
                category.num_words + name.num_words + 1  # 1 word for header.
            )
            arg_words = arg_count * words_per_arg

            # Counter events can store up to 15 args only.
            assert arg_count <= 15

            # Emit the counter track
            merged_trace.write(
                counter_event_header(
                    name.ref,
                    category.ref,
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
            assert (
                timestamp_ticks >= 0
            ), f"timestamp_ticks must be positive: {timestamp_ticks} {sample.__dict__}"
            merged_trace.write(timestamp_ticks.to_bytes(8, "little"))
            merged_trace.write(category.data)
            merged_trace.write(name.data)

            def double_argument_header(name_ref: int, size: int) -> int:
                argument_type = 5
                return name_ref << 16 | size << 4 | argument_type

            def write_double_argument(
                trace_file: BinaryIO, name: str, data: float
            ) -> None:
                # Words are 8 bytes. 2 for header and data. Variable for name.
                name_string = InlineString(name)
                words_total = 2 + name_string.num_words
                trace_file.write(
                    double_argument_header(
                        name_string.ref, words_total
                    ).to_bytes(8, "little")
                )
                trace_file.write(name_string.data)
                s = struct.pack("d", data)
                trace_file.write(s)

            write_double_argument(merged_trace, "Voltage", sample.voltage)
            write_double_argument(merged_trace, "Current", sample.current)
            write_double_argument(merged_trace, "Power", sample.power)

            if sample.aux_current is not None:
                write_double_argument(
                    merged_trace, "Raw Aux", sample.aux_current
                )

            # Write the counter_id, taken as the power rail's ID.
            merged_trace.write(sample.rail_id.to_bytes(8, "little"))


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
    model: trace_model.Model, power_samples: Sequence[Sample], fxt_path: str
) -> None:
    """Merges power_samples into model, placing merged trace at fxt_path.

    NB: model MUST be the in-memory representation of the serialized trace stored at fxt_path.
        This function merely appends data to fxt_path; it cannot write trace events other than
        synthetic events representing the data from power_samples.

    Upon return, the serialized trace at fxt_path will include the events from model along with
    synthesized events representing the data in power_samples.

    Args:
        model: The in-memory representation of the trace model stored at fxt_path.
        power_samples: Power readings to merge with model.
        fxt_path: A path to a serialized trace which has been read into memory and provided in
                  model.
    """
    # We'll start by reading in the fuchsia cpu data from the trace model
    scheduling_intervals = _read_fuchsia_trace_cpu_usage(model)

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
    # separated by more than a second.

    # Due to limits on the maximum size of our traces, there is potential for particularly busy
    # traces to not contain the entire first 5 seconds of the test. In order to ensure the
    # correlation logic operates correctly, we need to compare consistent counts of samples for
    # power and CPU readings. Thus, we take the a subset of the two datasets limited to a maximum
    # of ~5 seconds worth of samples. (5khz * 5 seconds = 25000 samples) and compare it to the
    # first ~4 seconds (80% of ~5 seconds) of the opposite dataset to attempt to match them up.
    # This amounts to nominally comparing the first 4 seconds of power samples to the first 5
    # seconds of CPU readings, and then vice-versa.
    max_sequence_samples = 25000
    signal_samples = min(
        max_sequence_samples, len(avg_cpu_combined), len(power_samples)
    )

    current_samples = normalize([sample.current for sample in power_samples])
    avg_cpu_combined = normalize(avg_cpu_combined)

    # Ensures feature list is always shorter than the number of signal samples
    feature_samples = int(0.8 * signal_samples)
    (
        power_after_cpu_correlation,
        power_after_cpu_correlation_idx,
    ) = cross_correlate_arg_max(
        avg_cpu_combined[0:signal_samples],
        current_samples[0:feature_samples],
    )
    (
        cpu_after_power_correlation,
        cpu_after_power_correlation_idx,
    ) = cross_correlate_arg_max(
        current_samples[0:signal_samples],
        avg_cpu_combined[0:feature_samples],
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

    _append_power_data(fxt_path, power_samples, starting_ticks)


@dataclasses.dataclass(frozen=True)
class GonkSample:
    host_time: trace_time.TimePoint
    gonk_time: trace_time.TimePoint  # Computed from delta from previous sample.
    delta: trace_time.TimeDelta  # Time since last ADC read constituting this sample.
    voltages: list[float]
    currents: list[float]
    powers: list[float]
    pin_assert: int | None = None

    @classmethod
    def from_values(
        cls, vals: Sequence[str], prev_time: trace_time.TimePoint
    ) -> Self:
        """
        Create a new GonkSample based on a single line of CSV data.

        Args:
          `vals`: Sequence of strings representing a single sample. See below for format.
          `prev_time`: Time of the previous measurement, used to calculate this sample's
                       timestamp based on its delta_micros (microseconds since last sample).

        The vals list gets parsed as [host_time, delta_micros, samples..., comment].

        - `host_time` is a formatted time string. See implementation for parsing.
        - `delta_micros` is an integer string representing microseconds since the last sample.
        - `samples` are floating point strings in multiples of 3 (voltage, current, power).
        - `comment` is an arbitrary string. See implementation for parsing.

        The vals list format generally follows either of 2 archetypes: Sample or Comment.

        - *Sample* contains strings for host_time, delta_micros, samples. Comment string is empty.
        - *Comment* contains strings for host_time, delta_micros, comment. Sample strings are empty.

        Examples:

        - ['20240927 18:00:23.609548', '286', '0.8148437500000001', '1.010546875', '4.894726562500001', ... , '']
        - ['20240927 18:00:42.426986', '0', '', '', '', ... , 'Header pin assert: 2']
        """
        assert (
            len(vals) >= 3
        ), f"Values should contain at least host_time, delta_micros, and comment. vals={vals}"
        host_time_str = vals[0]
        host_time = trace_time.TimePoint.from_epoch_delta(
            trace_time.TimeDelta.from_seconds(
                datetime.datetime.strptime(
                    host_time_str, "%Y%m%d %H:%M:%S.%f"
                ).timestamp()
            )
        )
        delta = trace_time.TimeDelta.from_microseconds(int(vals[1]))
        # Samples are floating point strings or empty strings.
        samples = [float(s.strip() or "nan") for s in vals[2:-1]]
        comment = vals[-1]

        # Number of samples must be a multiple of 3 for voltage, current, power on n rails.
        assert (
            len(samples) % 3 == 0
        ), f"Number of samples must be a multiple of 3. samples={samples}"
        n = len(samples) // 3
        voltages = samples[:n]
        currents = samples[n : 2 * n]
        powers = samples[2 * n : 3 * n]

        # Set header pin assert value if available.
        match = re.search(r"Header pin assert: (\d+)", comment)
        pin_assert = int(match.group(1)) if match else None

        gonk_time = prev_time + delta
        return cls(
            host_time,
            gonk_time,
            delta,
            voltages,
            currents,
            powers,
            pin_assert,
        )


def read_gonk_header(csv_path: str) -> list[str]:
    """Get power rail names based on Gonk CSV header."""
    with open(csv_path, "r") as power_csv:
        reader = csv.reader(power_csv)
        headers = next(reader)
        headers = [h.strip() for h in headers]
        assert (
            headers[0] == "host_time"
        ), f"Header should be host_time: {headers[0]}"
        assert (
            headers[1] == "delta_micros"
        ), f"Header should be delta_micros: {headers[1]}"
        names = headers[2:-1]
        assert (
            headers[-1] == "comment"
        ), f"Header should be comment: {headers[-1]}"

        # Number of samples must be a multiple of 3 for voltage, current, power on n rails.
        assert (
            len(names) % 3 == 0
        ), f"Number of sample header columns must be a multiple of 3. names={names}"

        # Only look at the voltage samples to get the rail names (prefixed with 'V_').
        # Current and power samples are assumed to have similar names.
        num_rails = len(names) // 3

        return [
            n.lstrip("V_").replace("(", "_").replace(")", "").strip("_")
            for n in names[:num_rails]
        ]


def read_gonk_samples(csv_path: str) -> Iterator[GonkSample]:
    with open(csv_path, "r") as power_csv:
        reader = csv.reader(power_csv)
        # Skip the header.
        next(reader)
        prev_time = trace_time.TimePoint(0)
        for line in reader:
            sample = GonkSample.from_values(line, prev_time)
            prev_time = sample.gonk_time
            yield sample


def merge_gonk_data(
    model: trace_model.Model,
    gonk_samples: Iterable[GonkSample],
    fxt_path: str,
    rail_names: list[str],
) -> None:
    """Merges gonk_samples into model, placing merged trace at fxt_path.

    NB: model MUST be the in-memory representation of the serialized trace stored at fxt_path.
        This function merely appends data to fxt_path; it cannot write trace events other than
        synthetic events representing the data from gonk_samples.

    Upon return, the serialized trace at fxt_path will include the events from model along with
    synthesized events representing the data in gonk_samples.

    Args:
        model: The in-memory representation of the trace model stored at fxt_path.
        gonk_samples: Gonk power readings to merge with model.
        fxt_path: A path to a serialized trace which has been read into memory and provided in
                  model.
    """
    # Search trace data for at least 2 high-to-low transitions on the specified gpio header pin.
    lows_trace = [
        e
        for e in model.all_events()
        if e.category == "gpio" and e.name == "set-low"
    ]
    assert (
        len(lows_trace) >= 2
    ), f"Trace must contain at least 2 low transitions. Found: {lows_trace}"

    # Search gonk samples for at least 2 pin asserts, each corresponding to a high-to-low transition.
    lows_gonk = []
    gonk_samples_filtered = []
    for s in gonk_samples:
        if s.pin_assert:
            lows_gonk.append(s)
        else:
            gonk_samples_filtered.append(s)
    assert (
        len(lows_gonk) >= 2
    ), f"Gonk data must contain at least 2 low transitions. Found: {lows_gonk}"
    assert len(lows_gonk) == len(
        lows_trace
    ), f"Gonk and trace data must have the same number of transitions. Found: lows_gonk={lows_gonk} lows_trace={lows_trace}"

    # Gonk and trace data aren't guaranteed to be sorted. Make sure to sort by time ascending.
    lows_gonk.sort(key=lambda sample: sample.host_time)
    lows_trace.sort(key=lambda event: event.start)

    # Map high-to-low transitions in Gonk to align with trace.
    base_trace: trace_time.TimePoint = lows_trace[0].start
    delta_trace: trace_time.TimeDelta = lows_trace[1].start - base_trace
    base_gonk: trace_time.TimePoint = lows_gonk[0].gonk_time
    end_gonk: trace_time.TimePoint = lows_gonk[1].gonk_time
    delta_gonk: trace_time.TimeDelta = end_gonk - base_gonk

    assert (
        delta_trace.to_nanoseconds() >= 0
    ), f"delta_trace should be positive: {delta_trace} {lows_trace}"
    assert (
        delta_gonk.to_nanoseconds() >= 0
    ), f"delta_gonk should be positive: {delta_gonk} {lows_gonk}"

    def gonk_time_to_trace_delta(
        sample_time: trace_time.TimePoint,
    ) -> trace_time.TimeDelta:
        sample_delta_gonk = sample_time - base_gonk
        sample_delta_trace = trace_time.TimeDelta(
            int(
                sample_delta_gonk.to_nanoseconds()
                * delta_trace.to_nanoseconds()
                / delta_gonk.to_nanoseconds()
            )
        )
        return sample_delta_trace

    # Trace timestamp where the first high-to-low transition occurred.
    base_trace_ns = base_trace.to_epoch_delta().to_nanoseconds()

    # Adjust each GonkSample's time using computed clock drift.
    power_samples_for_trace = []
    for s in gonk_samples_filtered:
        time_offset = gonk_time_to_trace_delta(s.gonk_time)
        n_rails = len(s.voltages)
        for i in range(n_rails):
            timestamp = base_trace_ns + time_offset.to_nanoseconds()

            # Skip samples that were adjusted backwards past the beginning of the trace.
            if timestamp < 0:
                continue

            power_samples_for_trace.append(
                Sample(
                    timestamp,
                    s.currents[i],
                    s.voltages[i],
                    aux_current=None,
                    power=s.powers[i],
                    rail_id=(i + 1),
                    rail_name=rail_names[i],
                )
            )

    # Insert gonk samples with the modified timestamp into the existing trace.
    _append_power_data(fxt_path, power_samples_for_trace, starting_ticks=0)
