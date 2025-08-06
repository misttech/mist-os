#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import shutil
from argparse import ArgumentParser, BooleanOptionalAction, Namespace
from collections import defaultdict
from dataclasses import dataclass
from os import environ
from pathlib import Path
from subprocess import CalledProcessError, run
from sys import argv, stderr
from typing import Any, Optional

from rust import HOST_PLATFORM, GnTarget

# run ninja to build all rustdoc targets. --no-build will break this
# script unless the third party rust targets are already manually built


@dataclass(frozen=True)
class Metadata:
    """
    Metadata collected for one of our rustdoc targets from the
    rust_target_mapping walk.
    """

    crate_name: str  # according to --extern
    actual_label: str
    rustdoc_label: str
    rustdoc_out_dir: Path
    disable_rustdoc: bool
    original_label: str
    rustdoc_parts_dir: str
    target_is_fuchsia: bool

    def is_shared_target(self) -> bool:
        return self.actual_label.endswith("-shared)")

    @staticmethod
    def parse(m: dict[str, Any], args: Namespace) -> Optional["Metadata"]:
        if "extern" not in m:
            return None
        _, crate_name, _ = m["extern"].split("=")
        rustdoc_out_dir = Path(args.build_dir, m["rustdoc_out_dir"])
        return Metadata(
            crate_name=crate_name,
            actual_label=m["actual_label"],
            rustdoc_label=m["rustdoc_label"],
            rustdoc_out_dir=rustdoc_out_dir,
            disable_rustdoc=m["disable_rustdoc"],
            original_label=m["original_label"],
            rustdoc_parts_dir=m["rustdoc_parts_dir"],
            target_is_fuchsia=m["target_is_fuchsia"],
        )


def read_metadata_file(args: Namespace) -> set[Metadata]:
    """Reads rust_target_mapping.json to get metadata"""
    meta = args.rust_target_mapping.read_bytes()
    meta = json.loads(meta)
    return {Metadata.parse(m, args) for m in meta} - {None}


def fx_build_all(meta: list[Metadata], args: Namespace):
    """
    Build docs for the rustdoc targets that appear in `meta`. These may not
    have been documented during a normal `fx build` run.

    Raises:
        CalledProcessError if the the build fails.
    """
    rustdoc_labels = [m.rustdoc_label for m in meta]
    if len(rustdoc_labels) == 0:
        print("fx rustdoc-link: found no rustdoc labels to build", file=stderr)
        return
    if args.use_xargs:
        # We use xargs to avoid calling fx build with too many arguments. xargs checks
        # the environment for maximum argument list size and may potentially make
        # several calls to fx build in the case that we have too many to fit in one.
        rustdoc_labels = "".join(f"{l}\0" for l in rustdoc_labels)
        result = run(
            ["xargs", "--null", args.build_executable],
            text=True,
            input=rustdoc_labels,
            cwd=args.build_dir,
        )
    else:
        result = run(
            [args.build_executable, *rustdoc_labels],
            cwd=args.build_dir,
        )
    result.check_returncode()


def filter_build_labels(
    meta: list[Metadata],
    build_dir: Path,
    user_labels: list[str],
    print_build_labels: bool,
) -> list[Metadata]:
    """
    If the user provides specific labels to build, we should only build those.
    """
    assert (
        len(user_labels) > 0
    ), "only call this method if the user provided specific labels to build"
    user_labels = [str(GnTarget(l, build_dir)) for l in user_labels]

    filtered_meta = []
    for m in meta:
        possible_labels = {
            str(GnTarget(m.rustdoc_label, build_dir)),
            str(GnTarget(m.original_label, build_dir)),
            str(GnTarget(m.actual_label, build_dir)),
        }
        matching = not possible_labels.isdisjoint(user_labels)
        if matching:
            filtered_meta.append(m)

    if print_build_labels:
        print("building specific labels", *user_labels, file=stderr)
    return filtered_meta


def dedup_crate_name(
    meta: list[Metadata], print_collisions: bool
) -> list[Metadata]:
    """remove metadata for duplicate crate names"""
    ret = defaultdict(list)
    for m in meta:
        ret[m.crate_name, m.target_is_fuchsia].append(m)

    for (name, _target_is_fuchsia), l in ret.items():
        if len(l) > 1 and print_collisions:
            print(f"crate_name collision: {name}", file=stderr)
            for m in sorted(l, key=lambda m: m.actual_label):
                print(f"  {m.actual_label}", file=stderr)

    # Return alphabetically greatest crates because these are somewhat more likely to be the latest
    # version. We can only choose one version of a crate to document due to rustdoc restrictions
    # about having multiple crates with the same name.
    return [max(l, key=lambda m: m.actual_label) for l in ret.values()]


def rustdoc_link(
    dst: Path,
    argfile: Path,
    extra_rustdoc_args: list[str],
    rustdoc_executable: Path,
    meta: list[Metadata],
    use_xargs: bool,
    quiet: bool,
):
    """
    Run rustdoc --merge=finalize to output cross crate rustdoc information,
    and copy all of the docs to a central location.

    We call this function once for the host docs, and once for the fuchsia docs.

    Raises:
        CalledProcessError if rustdoc or copy operation fails.
    """

    include_parts_dir_args = [
        f"--include-parts-dir={m.rustdoc_parts_dir}" for m in meta
    ]

    Path(args.build_dir, dst).mkdir(parents=True, exist_ok=True)
    argfile.parent.mkdir(parents=True, exist_ok=True)

    flags = [
        f"--out-dir={dst}",
        "--edition=2021",
        "-Zunstable-options",
        "--merge=finalize",
        *include_parts_dir_args,
        *extra_rustdoc_args,
    ]
    argfile.write_text("\n".join(flags))

    # Run rustdoc.
    result = run([rustdoc_executable, f"@{argfile}"], cwd=args.build_dir)
    result.check_returncode()

    if not quiet:
        print(
            "fx rustdoc-link: copying docs to",
            dst,
            file=stderr,
        )

    # Use a trailing slash dot to copy the contents of the doc
    # directory instead of the directory itself.
    sources = set(f"{m.rustdoc_out_dir}/." for m in meta)
    if use_xargs:
        sources = "".join(f"{s}\0" for s in sources)
        # Let's merge the files into our destination. We use xargs here to take
        # advantage of parallelism. It is much (35%) faster than packing everything
        # into a single cp invocation. It is much much (75%) faster than shutil.copytree.
        result = run(
            [
                "xargs",
                "--no-run-if-empty",
                "--null",
                "--max-args=1",
                "--max-procs",
                str(args.merge_parallelism),
                "cp",
                "--recursive",
                "--target-directory",
                dst,
            ],
            cwd=args.build_dir,
            text=True,
            input=sources,
        )
    else:
        result = run(
            [
                "cp",
                "--recursive",
                "--target-directory",
                dst,
                *sources,
            ],
            cwd=args.build_dir,
        )
    result.check_returncode()


def main(args: Namespace) -> None:
    """
    Extract rustdoc targets, build them, and copy the docs to the --destination.
    """
    set_arg_defaults(args)
    meta = read_metadata_file(args)

    # remove shared and disabled targets
    meta = [
        m for m in meta if not m.is_shared_target() and not m.disable_rustdoc
    ]

    # filter according to the specific targets that the user wants to build
    if args.build_labels:
        meta = filter_build_labels(
            meta,
            args.build_dir,
            args.build_labels,
            print_build_labels=args.verbose,
        )

    meta = dedup_crate_name(meta, print_collisions=args.verbose)

    if args.build:
        try:
            fx_build_all(meta, args)
        except CalledProcessError as e:
            print(
                f"Build failed... pass --no-build to still attempt to link the rustdoc outputs: {e}",
                file=stderr,
            )
            return

    argfiles = Path(args.build_dir, "docs/rust/argfiles")

    # run rustdoc to merge documentation and copy host docs to the central --destination
    try:
        rustdoc_link(
            meta=[
                m
                for m in meta
                if not m.disable_rustdoc and not m.target_is_fuchsia
            ],
            dst=Path(args.destination, "host"),
            argfile=Path(argfiles, "host.args"),
            rustdoc_executable=args.rustdoc_executable,
            extra_rustdoc_args=args.extra_rustdoc_arg,
            use_xargs=args.use_xargs,
            quiet=args.quiet,
        )
        rustdoc_link(
            meta=[
                m for m in meta if not m.disable_rustdoc and m.target_is_fuchsia
            ],
            dst=args.destination,
            argfile=Path(argfiles, "fuchsia.args"),
            rustdoc_executable=args.rustdoc_executable,
            extra_rustdoc_args=args.extra_rustdoc_arg,
            use_xargs=args.use_xargs,
            quiet=args.quiet,
        )
    except CalledProcessError as e:
        print(
            f"fx rustdoc-link: merge operation failed: {e}",
            file=stderr,
        )
        return

    if args.zip_to is not None:
        zip_to_without_suffix = args.zip_to.with_suffix("")
        shutil.make_archive(
            zip_to_without_suffix, format="zip", root_dir=args.destination
        )
        assert Path(
            args.zip_to
        ).is_file(), "should have created .zip with our intended name"


def set_arg_defaults(args: Namespace) -> None:
    """
    need to set defaults here because they depend on the fuchsia_dir and build_dir args
    """
    assert (
        args.fuchsia_dir is not None
    ), "must provide fuchsia dir through --fuchsia-dir or FUCHSIA_DIR envirnoment var. This is normally done through the devshell wrapper."
    assert (
        args.build_dir is not None
    ), "must provide build dir through --build-dir or FUCHSIA_BUILD_DIR environment var. This is normally done through the devshell wrapper."

    if args.rust_target_mapping is None:
        args.rust_target_mapping = Path(
            args.build_dir, "rust_target_mapping.json"
        )
    if args.destination is None:
        args.destination = Path(args.build_dir, "docs/rust/doc")
    if args.rustdoc_executable is None:
        args.rustdoc_executable = Path(
            args.fuchsia_dir,
            "prebuilt/third_party/rust",
            HOST_PLATFORM,
            "bin/rustdoc",
        )
    if args.build_executable is None:
        args.build_executable = Path(args.fuchsia_dir, "tools/devshell/build")
    if args.extra_rustdoc_arg is None:
        args.extra_rustdoc_arg = []


def _main_arg_parser() -> ArgumentParser:
    parser = ArgumentParser(
        description="Documents all (subject to `build_only_labels`) rust crates, and merges the docs to a single destination."
    )
    parser.add_argument(
        "--fuchsia-dir",
        default=environ.get("FUCHSIA_DIR"),
        type=str,
        help="root fuchsia directory, e.g. ~/fuchsia. Typically provided by the devshell wrapper. Can be relative to the current working directory.",
    )
    parser.add_argument(
        "--build-dir",
        default=environ.get("FUCHSIA_BUILD_DIR"),
        type=str,
        help="build directory to rebase path to, e.g. $FUCHSIA_DIR/out/default. Typically provided by the devshell wrapper. Cana be relative to the current working directory.",
    )
    parser.add_argument(
        "--rust-target-mapping",
        type=Path,
        help="path to json array of rustdoc target info. Defaults to $FUCHSIA_BUILD_DIR/rust_target_mapping.json",
    )
    parser.add_argument(
        "--destination",
        type=Path,
        help="Directory to place merged documentation. Can be relative to the current working directory.",
    )
    parser.add_argument(
        "--rustdoc-executable",
        help="path directly to the rustdoc executable. Can be relative to the current working directory.",
    )
    parser.add_argument(
        "--build",
        default=True,
        action=BooleanOptionalAction,
        help="run fx build to build extra rustdoc targets. --no-build will break this script unless the third party rust targets are already manually built",
    )
    parser.add_argument(
        "--quiet",
        default=False,
        action=BooleanOptionalAction,
        help="Do not print status messages. Failure messages still printed.",
    )
    parser.add_argument(
        "--verbose",
        default=False,
        action=BooleanOptionalAction,
        help="add additional context to status messages",
    )
    parser.add_argument(
        "--build-executable",
        type=Path,
        help="path to fx build program. Defaults to the in-tree source of `fx build`",
    )
    parser.add_argument(
        "--merge-parallelism",
        default=os.cpu_count(),
        type=int,
        help="How much parallelism to use in scheduling the tasks that merge docs",
    )
    parser.add_argument(
        "--out-dir",
        help="ignored",
    )
    parser.add_argument(
        "--use-xargs",
        default=True,
        action=BooleanOptionalAction,
        help="use xargs for parallelism and splitting argument lists",
    )
    parser.add_argument(
        "--zip-to",
        type=Path,
        help="For testing. Zip generated docs to a specified location. Can be relative to the current working directory.",
    )
    parser.add_argument(
        "--extra-rustdoc-arg",
        action="append",
        help="provide extra arguments to rustdoc",
    )
    parser.add_argument(
        "build_labels",
        nargs="*",
        help="build only these labels if supplied",
    )
    parser.set_defaults(func=main)
    return parser


if __name__ == "__main__":
    parser = _main_arg_parser()
    args = parser.parse_args(argv[1:])
    print(args.build_dir)
    args.func(args)
