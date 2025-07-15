#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Runs clippy on a set of gn targets or rust source files

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from rust import (
    FUCHSIA_BUILD_DIR,
    HOST_PLATFORM,
    PREBUILT_THIRD_PARTY_DIR,
    GnTarget,
)


def possible_labels(t: dict[str, any], fuchsia_dir: str) -> set[str]:
    """all labels the user could have meant"""
    return [
        str(GnTarget(t["clippy_label"], fuchsia_dir)),
        str(GnTarget(t["original_label"], fuchsia_dir)),
        str(GnTarget(t["actual_label"], fuchsia_dir)),
    ]


def main():
    args = parse_args()

    build_dir = Path(args.out_dir) if args.out_dir else FUCHSIA_BUILD_DIR

    rust_target_mapping = Path(
        build_dir, "rust_target_mapping.json"
    ).read_bytes()
    rust_target_mapping = json.loads(rust_target_mapping)
    rust_target_mapping = [
        t for t in rust_target_mapping if not t["disable_clippy"]
    ]
    if not args.all:
        # filter rust target mapping
        if args.files:
            input_files = {os.path.relpath(f, build_dir) for f in args.input}
            rust_target_mapping = [
                t
                for t in rust_target_mapping
                for s in t["src"]
                if s in input_files
            ]
        else:  # they are labels
            targets_look_like_sources = [
                t for t in args.input if t.endswith(".rs")
            ]
            if targets_look_like_sources:
                print(
                    f"Warning: {targets_look_like_sources} looks like a source file rather than a target, "
                    "maybe you meant to use --files ?"
                )
            rust_target_mapping = [
                t
                for t in rust_target_mapping
                for l in args.input
                if str(GnTarget(l, args.fuchsia_dir))
                in possible_labels(t, build_dir)
            ]

    clippy_outputs = list(
        set(str(Path(t["clippy_output"])) for t in rust_target_mapping)
    )

    if args.get_outputs:
        print(*clippy_outputs, sep="\n")
        return 0

    if not clippy_outputs:
        print("Error: Couldn't find any clippy outputs for those inputs")
        return 1

    if args.no_build:
        run_time = 0
        returncode = 0
    else:
        run_time = time.time()
        returncode = build_targets(clippy_outputs, build_dir, args).returncode

    lints = {}
    for clippy_output in clippy_outputs:
        clippy_output = build_dir / clippy_output
        # If we failed to build all targets we can keep going and print any
        # lints that were collected.
        if returncode != 0:
            if not clippy_output.exists():
                continue
            if os.path.getmtime(clippy_output) < run_time:
                continue
        with open(clippy_output) as f:
            error_reported = False
            for line in f:
                try:
                    lint = json.loads(line)
                except json.decoder.JSONDecodeError:
                    if not error_reported:
                        print(f"Malformed output: {clippy_output}")
                        returncode = 1
                        error_reported = True
                    continue
                # filter out "n warnings emitted" messages
                if not lint["spans"]:
                    continue
                lints[fingerprint_diagnostic(lint)] = lint

    if not args.raw:
        print("\nClippy Diagnostics\n==================\n")
    for lint in lints.values():
        print(json.dumps(fix_paths(lint)) if args.raw else lint["rendered"])
    if not args.raw:
        print(len(lints), "warning(s) emitted\n")

    return returncode


# To deduplicate lints, use the message, code, all top level spans, and macro
# expansion spans
def fingerprint_diagnostic(lint):
    code = lint.get("code")

    def expand_spans(span):
        yield span
        if expansion := span.get("expansion"):
            yield from expand_spans(expansion["span"])

    spans = [x for span in lint["spans"] for x in expand_spans(span)]

    return (
        lint["message"],
        code.get("code") if code else None,
        frozenset(
            (x["file_name"], x["byte_start"], x["byte_end"]) for x in spans
        ),
    )


# Rewrite paths in a diagnostic to be relative to the current directory.
def fix_paths(lint):
    fix = lambda path: os.path.relpath(os.path.join(FUCHSIA_BUILD_DIR, path))
    for span in lint["spans"]:
        span["file_name"] = fix(span["file_name"])
    lint["children"] = [fix_paths(child) for child in lint["children"]]
    return lint


def build_targets(clippy_outputs, build_dir, args):
    prebuilt = PREBUILT_THIRD_PARTY_DIR
    if args.fuchsia_dir:
        prebuilt = Path(args.fuchsia_dir) / "prebuilt" / "third_party"
    ninja = [
        prebuilt / "ninja" / HOST_PLATFORM / "ninja",
        "-C",
        build_dir,
    ]
    if args.stop_on_first_error:
        ninja += ["-k", "1"]
    else:
        ninja += ["-k", "0"]
    if args.jobs is not None:
        ninja += ["-j", str(args.jobs)]

    if args.verbose:
        ninja += ["--verbose"]
    if args.quiet:
        ninja += ["--quiet"]
    ninja += clippy_outputs
    output = sys.stderr if args.raw else None
    env = os.environ
    env.setdefault("NINJA_PERSISTENT_MODE", "1")
    if args.verbose:
        print(" ".join([str(arg) for arg in ninja]))
    return subprocess.run(ninja, stdout=output, env=env)


def parse_args():
    parser = argparse.ArgumentParser(
        description="Run cargo clippy on a set of targets or rust files"
    )
    parser.add_argument(
        "--verbose",
        "-v",
        help="verbose build output",
        action="store_true",
        default=False,
    )
    parser.add_argument(
        "--quiet",
        help="don't show progress status",
        action="store_true",
        default=False,
    )
    parser.add_argument(
        "--files",
        "-f",
        action="store_true",
        help="treat the inputs as source files rather than gn targets",
    )
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("input", nargs="*", default=[])
    inputs.add_argument(
        "--all", action="store_true", help="run on all clippy targets"
    )
    advanced = parser.add_argument_group("advanced")
    advanced.add_argument(
        "--out-dir", help="path to the Fuchsia build directory"
    )
    advanced.add_argument(
        "--fuchsia-dir", help="path to the Fuchsia root directory"
    )
    advanced.add_argument(
        "--raw",
        action="store_true",
        help="emit full json rather than human readable messages",
    )
    advanced.add_argument(
        "--get-outputs",
        action="store_true",
        help="emit a list of clippy output files rather than lints",
    )
    advanced.add_argument(
        "--no-build",
        action="store_true",
        help="don't build the clippy output, instead expect that it already exists",
    )
    advanced.add_argument(
        "--stop-on-first-error",
        action="store_true",
        help="stop on first error",
        default=False,
    )
    advanced.add_argument(
        "--jobs",
        "-j",
        type=int,
        help="number of concurrent jobs to run",
        default=None,
    )
    return parser.parse_args()


if __name__ == "__main__":
    sys.exit(main())
