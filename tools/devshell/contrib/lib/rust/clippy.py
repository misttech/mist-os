#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Runs clippy on a set of gn targets or rust source files

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import rust
from rust import FUCHSIA_BUILD_DIR, HOST_PLATFORM, PREBUILT_THIRD_PARTY_DIR


def main():
    args = parse_args()
    build_dir = Path(args.out_dir) if args.out_dir else FUCHSIA_BUILD_DIR
    generated_file = build_dir / "clippy_target_mapping.json"

    if args.all:
        clippy_targets = get_targets(
            generated_file, set(), build_dir, get_all=True
        )
    elif args.files:
        input_files = {os.path.relpath(f, build_dir) for f in args.input}
        clippy_targets = get_targets(generated_file, input_files, build_dir)
        if args.verbose and not args.get_outputs:
            print("Found the following targets for those source files:")
            print(*(t.gn_target for t in clippy_targets), sep="\n")
    else:
        clippy_targets = []
        for target in args.input:
            if target.endswith(".rs"):
                print(
                    f"Warning: {target} looks like a source file rather than a target, "
                    "maybe you meant to use --files ?"
                )
            gn_target = rust.GnTarget(target, args.fuchsia_dir)
            gn_target.label_name += ".clippy"
            clippy_targets.append(gn_target)

    output_files = [
        os.path.relpath(t.gen_dir(build_dir).joinpath(t.label_name), build_dir)
        for t in clippy_targets
    ]
    if args.get_outputs:
        print(*output_files, sep="\n")
        return 0
    if not output_files:
        print("Error: Couldn't find any clippy outputs for those inputs")
        return 1
    if args.no_build:
        run_time = 0
        returncode = 0
    else:
        run_time = time.time()
        returncode = build_targets(
            output_files,
            build_dir,
            args.fuchsia_dir,
            args.verbose,
            args.quiet,
            args.raw,
        ).returncode

    lints = {}
    for clippy_output in output_files:
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
                # filter out lints for files we didn't ask for
                if args.files and all(
                    span["file_name"] not in input_files
                    for span in lint["spans"]
                ):
                    continue
                lints[fingerprint_diagnostic(lint)] = lint

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


def build_targets(output_files, build_dir, fuchsia_dir, verbose, quiet, raw):
    prebuilt = PREBUILT_THIRD_PARTY_DIR
    if fuchsia_dir:
        prebuilt = Path(fuchsia_dir) / "prebuilt" / "third_party"
    ninja = [
        prebuilt / "ninja" / HOST_PLATFORM / "ninja",
        "-C",
        build_dir,
        "-k",
        "0",
    ]
    if verbose:
        ninja += ["--verbose"]
    if quiet:
        ninja += ["--quiet"]
    output = sys.stderr if raw else None
    env = os.environ
    env.setdefault("NINJA_PERSISTENT_MODE", "1")
    return subprocess.run(ninja + output_files, stdout=output, env=env)


def get_targets(source_map, input_files, build_dir, get_all=False):
    targets = set()
    with open(source_map) as f:
        raw = json.load(f)
    for target in raw:
        clippy_target = rust.GnTarget(target["clippy"], build_dir)
        if get_all or any(f in input_files for f in target["src"]):
            targets.add(clippy_target)
    return targets


def parse_args():
    parser = argparse.ArgumentParser(
        description="Run cargo clippy on a set of targets or rust files"
    )
    parser.add_argument(
        "--verbose", "-v", help="verbose", action="store_true", default=False
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
    return parser.parse_args()


if __name__ == "__main__":
    sys.exit(main())
