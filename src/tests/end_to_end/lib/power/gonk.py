#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Gonk power test utility library."""

import csv
import dataclasses
import datetime
import re
from collections.abc import Iterable, Iterator
from typing import Self, Sequence

from power import monsoon
from trace_processing import trace_model, trace_time


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
                monsoon.Sample(
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
    monsoon._append_power_data(
        fxt_path, power_samples_for_trace, starting_ticks=0
    )


def merge_gonk_data_without_gpio(
    gonk_samples: Iterable[GonkSample],
    fxt_path: str,
    rail_names: list[str],
) -> None:
    power_samples_for_trace = []
    for s in gonk_samples:
        n_rails = len(s.voltages)
        for i in range(n_rails):
            power_samples_for_trace.append(
                monsoon.Sample(
                    s.gonk_time.to_epoch_delta().to_nanoseconds(),
                    s.currents[i],
                    s.voltages[i],
                    aux_current=None,
                    power=s.powers[i],
                    rail_id=(i + 1),
                    rail_name=rail_names[i],
                )
            )
    monsoon._append_power_data(
        fxt_path, power_samples_for_trace, starting_ticks=0
    )
