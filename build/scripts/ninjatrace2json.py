#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Collect Ninja trace information for analysis in chrome://tracing."""

import argparse
import dataclasses
import json
import os
import subprocess
from concurrent import futures
from pathlib import Path
from typing import Any

JsonTrace = dict[str, Any]

NINJA_LOG_BASENAME = ".ninja_log"
COMPDB_BASENAME = "compdb.json"
GRAPH_BASENAME = "graph.dot"
NINJATRACE_BASENAME = "ninjatrace.json"
NINJA_SUBBUILDS_JSON = "ninja_subbuilds.json"


def _subbuild_ninja_target(build_dir: Path) -> str:
    """Returns the name of the ninja target from the main build, based on the
    subbuild directory."""
    # LINT.IfChange
    return str(build_dir.name) + ".stamp"
    # LINT.ThenChange(//build/subbuild.gni)


@dataclasses.dataclass
class Tracer:
    """Helper class that closes over some static configuration."""

    ninja_path: Path
    ninjatrace_path: Path
    rbe_rpl_path: Path
    rpl2trace_path: Path
    save_temps: bool

    def trace_build_dir(
        self,
        build_dir: Path,
        extra_ninja_targets: list[str],
    ) -> list[JsonTrace]:
        """Generate the Ninja trace for a single build directory (either the
        main build or a subbuild).

        The resulting trace will be written to `build_dir / ninjatrace.json`,
        but will also be parsed and returned."""
        ninja_log = build_dir / NINJA_LOG_BASENAME
        compdb = build_dir / COMPDB_BASENAME
        graph = build_dir / GRAPH_BASENAME
        trace = build_dir / NINJATRACE_BASENAME

        try:
            subprocess.run(
                [self.ninja_path, "-C", build_dir, "-t", "compdb"],
                stdout=compdb.open("w"),
                check=True,
            )
            # Note: only when specifying `-t graph` do we need the extra ninja
            # targets.
            subprocess.run(
                [
                    self.ninja_path,
                    "-C",
                    build_dir,
                    "-t",
                    "graph",
                    *extra_ninja_targets,
                ],
                stdout=graph.open("w"),
                check=True,
            )

            ninjatrace_args: list[str | os.PathLike[str]] = [
                self.ninjatrace_path,
                "-ninjalog",
                ninja_log,
                "-compdb",
                compdb,
                "-graph",
                graph,
                "-trace-json",
                trace,
                "-critical-path",
            ]

            if self.rbe_rpl_path:
                ninjatrace_args.extend(
                    [
                        "-rbe-rpl-path",
                        self.rbe_rpl_path,
                        "-rpl2trace-path",
                        self.rpl2trace_path,
                    ]
                )
            subprocess.run(ninjatrace_args, check=True)

            # Load and return the resulting trace file.
            with trace.open() as f:
                return json.load(f)
        finally:
            if not self.save_temps:
                compdb.unlink(missing_ok=True)
                graph.unlink(missing_ok=True)

    def find_and_merge_subbuilds(
        self, main_build_dir: Path, main_build_traces: list[JsonTrace]
    ) -> list[JsonTrace]:
        """Given the main build dir and its trace, find any subbuilds referenced
        by that trace, build them, load them, and merge them into a single
        trace file."""

        # ninja_subbuilds.json is a build API module listing the set of
        # directories that _could_ contain subbuilds, but those subbuilds may
        # not have actually run as part of the last build.
        with (main_build_dir / NINJA_SUBBUILDS_JSON).open() as f:
            possible_subbuild_dirs = [
                Path(b["build_dir"]) for b in json.load(f)
            ]

        traces_by_name = {t["name"]: t for t in main_build_traces}

        # Filter out subbuild dirs that aren't referenced by the main trace
        # file.
        filtered_subbuild_dirs = [
            b
            for b in possible_subbuild_dirs
            if _subbuild_ninja_target(b) in traces_by_name
        ]

        # Generate traces for the subbuild dirs in parallel
        pool = futures.ThreadPoolExecutor()
        subbuild_dir_and_traces = pool.map(
            lambda subbuild_dir: (
                subbuild_dir,
                # Subbuilds don't need extra targets (as of this writing).
                self.trace_build_dir(
                    build_dir=main_build_dir / subbuild_dir,
                    extra_ninja_targets=[],
                ),
            ),
            filtered_subbuild_dirs,
        )

        merged_traces = [*main_build_traces]
        for subbuild_dir, subbuild_traces in subbuild_dir_and_traces:
            target_in_main_build_trace = traces_by_name[
                _subbuild_ninja_target(subbuild_dir)
            ]
            assert target_in_main_build_trace, (
                "We already filtered out subbuilds not mentioned in the top-level build: %s"
                % subbuild_dir
            )

            for t in subbuild_traces:
                merged_traces += [
                    {
                        **t,
                        # Rewrite the trace to set "pid" to indicate the subbuild.
                        "pid": subbuild_dir.name,
                        # And offset the time by the start time of the subbuild
                        # action in the main build./
                        "ts": t["ts"] + target_in_main_build_trace["ts"],
                    }
                ]

        return merged_traces


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--extra-ninja-targets",
        nargs="*",
        help="""\
If you ran a full `fx build`, ignore this flag. If you built a specific set of
ninja targets (e.g. `fx build my_target other_target`), some of which aren't
depended on `//:default`, list those targets here. Otherwise they might not show
up in the resulting traces.""",
    )
    parser.add_argument(
        "--save-temps",
        action="store_true",
        help="""\
if set, keep the intermediate compdb.json and graph.dot files
in each build directory.  The are only needed temporarily to produce the
final ninjatrace.json, and can be large at O(100)s of MBs.""",
    )
    parser.add_argument(
        "--fuchsia-build-dir",
        type=Path,
        required=True,
        help="Path to the Fuchsia build directory.",
    )
    parser.add_argument(
        "--ninja-path",
        type=Path,
        required=True,
        help="Path to the prebuilt ninja binary.",
    )
    parser.add_argument(
        "--ninjatrace-path",
        type=Path,
        required=True,
        help="Path to the prebuilt ninjatrace binary.",
    )
    parser.add_argument(
        "--rbe-rpl-path",
        help="when provided, interleave remote execution stats from RBE into the main trace",
    )
    parser.add_argument(
        "--rpl2trace-path",
        type=Path,
        help="Path to the prebuilt rpl2trace tool.",
    )
    parser.add_argument(
        "--subbuilds-output-path",
        type=Path,
        help="""\
If set, merge traces from subbuilds with traces from the main build and write
the resulting trace file to this path. Must not be specified if
--subbuilds-in-place is set.""",
    )
    parser.add_argument(
        "--subbuilds-in-place",
        action="store_true",
        help="""\
If set, merge traces from subbuilds with traces from the main build and
include these traces in the main build's ninjatrace.json file. Must not be
specified if --subbuilds-output-path is set.""",
    )
    args = parser.parse_args()

    tracer = Tracer(
        ninja_path=args.ninja_path,
        ninjatrace_path=args.ninjatrace_path,
        rbe_rpl_path=args.rbe_rpl_path,
        rpl2trace_path=args.rpl2trace_path,
        save_temps=args.save_temps,
    )

    fuchsia_build_dir = args.fuchsia_build_dir

    main_build_traces = tracer.trace_build_dir(
        fuchsia_build_dir, args.extra_ninja_targets or []
    )

    if args.subbuilds_output_path or args.subbuilds_in_place:
        merged = tracer.find_and_merge_subbuilds(
            fuchsia_build_dir, main_build_traces
        )

        outpath = args.subbuilds_output_path or (
            fuchsia_build_dir / NINJATRACE_BASENAME
        )
        with outpath.open("w") as f:
            json.dump(merged, f)

    print(
        f"Now visit chrome://tracing and load {str(fuchsia_build_dir)}/ninjatrace.json"
    )


if __name__ == "__main__":
    main()
