#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from typing import Any, Dict, List

# Default cut-off for the percentage CPU. Any process that has CPU below this
# won't be listed in the results. User can pass in a cutoff.
DEFAULT_PERCENT_CUTOFF = 0.0


class AggCpuBreakdownMetricsProcessor:
    """
    Aggregates a given breakdown over the cores for each available frequency,
    and outputs in a free-form metrics format.
    """

    def __init__(
        self,
        # The json output from the cpu_breakdown script.
        breakdown: List[Dict[str, Any]],
        # A map from cpu numbers to their frequency.
        # e.g. { 0: 1.8, 1: 1.8, 2: 2.2, 3: 2.2, 4: : 2.2, 5: 2.2 }
        cpu_to_freq: Dict[int, float],
        total_time: float,
        percent_cutoff: float = DEFAULT_PERCENT_CUTOFF,
    ) -> None:
        # Output from the cpu_breakdown script.
        self._breakdown = breakdown
        # Transforms the frequency config to a map from cpu to frequency. Used
        # to determine which frequency each record should contribute its duration to.
        self._cpu_to_freq = cpu_to_freq
        self._percent_cutoff = percent_cutoff
        self._total_time = total_time

    def aggregate_metrics(self) -> Dict[float, List[Dict[str, Any]]]:
        """
        Given the breakdown of duration per thread, iterates through all the threads' durations for each
        CPU and aggregates them over each CPU frequency.
        """
        # Map from frequency to tid to aggregated duration.
        freq_to_tid_durs: Dict[float, Dict[int, float]] = {}
        # Tracks the final output. Contains a map from frequency to list of
        # threads with their durations and percentages.
        agg_breakdown: Dict[float, List[Dict[str, Any]]] = {}
        tid_to_thread_name: Dict[int, str] = {}
        tid_to_process_name: Dict[int, str] = {}
        for t in self._breakdown:
            # Save process and thread name for tid
            tid = t["tid"]
            tid_to_process_name[tid] = t["process_name"]
            tid_to_thread_name[tid] = t["thread_name"]

            # Get frequency for the cpu
            freq = self._cpu_to_freq[t["cpu"]]

            # Set the duration sum for the tid to 0 if nonexistent
            freq_to_tid_durs.setdefault(freq, {})
            freq_to_tid_durs[freq].setdefault(tid, 0)

            # Add the duration to the tid
            duration = t["duration"]
            freq_to_tid_durs[freq][tid] += duration

        for freq, tid_durs in freq_to_tid_durs.items():
            dur_list: List[Dict[str, Any]] = []
            for tid, dur in tid_durs.items():
                percent = (dur / self._total_time) * 100
                if percent >= self._percent_cutoff:
                    dur_list.append(
                        {
                            "process_name": tid_to_process_name[tid],
                            "thread_name": tid_to_thread_name[tid],
                            "duration": round(dur, 3),
                            "percent": round(percent, 3),
                        }
                    )
            agg_breakdown[freq] = sorted(
                dur_list,
                key=lambda m: (m["duration"]),
                reverse=True,
            )
        return agg_breakdown
