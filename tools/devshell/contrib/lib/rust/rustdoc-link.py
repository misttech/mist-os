#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
from argparse import ArgumentParser, BooleanOptionalAction, Namespace
from collections import defaultdict
from dataclasses import dataclass
from os import environ, link, symlink
from pathlib import Path
from shutil import copy, copytree
from subprocess import CalledProcessError, run
from sys import argv, stderr
from tempfile import TemporaryDirectory
from typing import Any, Callable, Optional

from rust import HOST_PLATFORM

# https://fxbug.dev/360942359: This script works, but will not generate cross-crate
# information. The search index, cross-crate trait impls, etc. are broken.
# Functionality using the flags from rust-lang/rfcs#3662 will be added in a
# future patch.

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
    target: str
    rustdoc_out_dir: Path
    rustdoc_parts_dir: Path
    touch: Path
    searchdir: str
    extern: str  # looks like --extern=CRATE_NAME=PATH
    disable_rustdoc: bool

    @property
    def extern_arg_as_rmeta(self) -> str:
        """Returns modified extern argument that points to the .rmeta"""
        flag, crate_name, path = self.extern.split("=")
        return "=".join(
            (flag, crate_name, str(Path(path).with_suffix(".rmeta")))
        )

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
            target=m["target"],
            touch=Path(f"{rustdoc_out_dir}.touch"),
            rustdoc_out_dir=rustdoc_out_dir,
            rustdoc_parts_dir=Path(args.build_dir, m["rustdoc_parts_dir"]),
            extern=m["extern"],
            searchdir=m["searchdir"],
            disable_rustdoc=m["disable_rustdoc"],
        )


def read_metadata_file(args: Namespace) -> set[Metadata]:
    """reads rust_target_mapping.json to get metadata"""
    meta = args.rust_target_mapping.read_bytes()
    meta = json.loads(meta)
    return {Metadata.parse(m, args) for m in meta} - {None}


def fx_build_all(meta: list[Metadata], args: Namespace):
    """
    Build docs for all rustdoc targets, including
    third_party crates. These may not have been documented
    during a normal `fx build` run.

    Raises:
        CalledProcessError if the the build fails.
    """
    rustdoc_labels = [m.rustdoc_label for m in meta if not m.disable_rustdoc]
    [m.actual_label for m in meta if not m.disable_rustdoc]
    # if `fx build` has no arguments, it will build everything. Avoid that.
    if len(rustdoc_labels) > 0:
        completed = run([args.build_executable] + rustdoc_labels)
        completed.check_returncode()
    else:
        print("did not find any !disable_rustdoc targets to build", file=stderr)


def group_by(items: list, group: Callable) -> dict[str, list]:
    """
    groups items by the hashable group returned by `group`
    """
    grouped = defaultdict(list)
    for m in items:
        grouped[group(m)].append(m)
    return grouped


def dedup_crate_name(meta: list[Metadata]) -> list[Metadata]:
    """remove metadata for duplicate crate names"""
    ret = defaultdict(list)
    for m in meta:
        ret[m.crate_name].append(m)

    for name, l in ret.items():
        if len(l) > 1 and not args.quiet:
            print(f"crate_name collision: {name}", file=stderr)
            for m in sorted(l, key=lambda m: m.actual_label):
                print(f"  {m.actual_label}", file=stderr)

    return [sorted(l, key=lambda m: m.actual_label)[0] for l in ret.values()]


def copy_tree(
    src: Path,
    dst: Path,
    src_touch: Path,
    copy_function: Callable,
    dst_touch: Path,
):
    """
    Wrap shutil.copytree, ignores exceptions and only copies upon stamp file changing.
    """
    dst_touch.mkdir(parents=True, exist_ok=True)
    dst_touch = dst_touch / str(src).replace("/", "%")
    dst_stat = None
    try:
        dst_stat = dst_touch.stat()
    except FileNotFoundError:
        # This file will not exist if it's the first time
        # running the copy operation for this target.
        # Will be created in this invocation of copy_tree.
        # Do not skip the copy.
        pass

    should_copy = (
        dst_stat is None or dst_stat.st_mtime < src_touch.stat().st_mtime
    )
    if not should_copy:
        return

    def remove_existing(s, d):
        """os.link and os.symlink fail if the target exists"""
        Path(d).unlink(missing_ok=True)
        copy_function(s, d)

    try:
        copytree(
            src=src,
            dst=dst,
            copy_function=remove_existing,
            dirs_exist_ok=True,
        )
        dst_touch.touch()

    except Exception as e:  # pylint: disable=broad-except
        # exceptions become warnings because
        # partially complete docs are better than no docs
        print(f"copying {src} to {dst} failed: {e}", file=stderr)


def rustdoc_link(meta: list[Metadata], args: Namespace):
    """Runs the rustdodc link phase for all targets."""

    assert (
        args.touch is None or args.destination is not None
    ), "--destination must be provided if --touch is provided"

    copy_function = {
        "symlink": symlink,
        "link": link,
        "copy": copy,
    }[args.copy_function]

    searchdir = set(m.searchdir for m in meta)

    for target, meta in group_by(meta, lambda m: m.target).items():
        meta = dedup_crate_name(meta)
        # filter disable_rustdoc late because we still want to
        # build deps marked disable_rustdoc for !disable_rustdoc targets
        meta = [m for m in meta if not m.disable_rustdoc]

        extra_rustdoc_args = (
            args.extra_rustdoc_args
            if args.extra_rustdoc_args is not None
            else []
        )

        dst = (
            args.destination
            if "fuchsia" in target
            else args.destination / "host"
        )

        extern = set(m.extern_arg_as_rmeta for m in meta)

        if not args.quiet:
            print("running rustdoc to document index for", target, file=stderr)
        with TemporaryDirectory() as tmp:
            crate_root = Path(tmp, "lib.rs")
            crate_names = [f"extern crate {m.crate_name};" for m in meta]
            rustdoc_parts_dir = [
                f"--include-parts-dir={m.rustdoc_parts_dir}" for m in meta
            ]
            crate_root.write_text("\n".join(crate_names))
            argfile = Path(tmp, "argfile")
            flags = [
                str(crate_root),
                *extern,
                *searchdir,
                "--crate-name=fuchsia_rustdoc_index",
                "--crate-type=lib",
                "--edition=2021",
                f"--target={target}",
                "-Zunstable-options",
                f"--out-dir={dst}",
                "--merge=finalize",
                *rustdoc_parts_dir,
                "--enable-index-page",
                *extra_rustdoc_args,
            ]
            argfile.write_text("\n".join(flags))
            completed = run(
                [args.rustdoc_executable, f"@{argfile}"], cwd=args.build_dir
            )
            completed.check_returncode()

        if not args.quiet:
            print("copying docs to destination", dst, file=stderr)
        for m in meta:
            copy_tree(
                src=m.rustdoc_out_dir,
                dst=dst,
                src_touch=m.touch,
                copy_function=copy_function,
                dst_touch=args.touch,
            )


def main(args: Namespace):
    set_arg_defaults(args)
    meta = read_metadata_file(args)

    # remove "shared" toolchain variants
    meta = list(m for m in meta if not m.actual_label.endswith("-shared)"))
    if args.build:
        try:
            fx_build_all(meta, args)
        except CalledProcessError:
            print(
                "Build failed... pass --no-build to still attempt to link the rustdoc outputs",
                file=stderr,
            )
            return
    rustdoc_link(meta, args)


def set_arg_defaults(args: Namespace):
    """
    need to set defaults here because they depend on the fuchsia_dir and build_dir args
    """
    assert (
        args.fuchsia_dir is not None
    ), "must provide fuchsia dir through --fuchsia-dir or FUCHSIA_DIR envirnoment var"
    assert (
        args.build_dir is not None
    ), "must provide build dir through --build-dir or FUCHSIA_BUILD_DIR environment var"

    if args.touch is None:
        args.touch = Path(args.build_dir, "docs/rust/touch")
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


def _main_arg_parser() -> ArgumentParser:
    parser = ArgumentParser(
        description="Documents all (subject to `build_only_labels`) rust crates, and merges the docs to a single destination."
    )
    parser.add_argument(
        "--fuchsia-dir",
        default=environ.get("FUCHSIA_DIR"),
        type=str,
        help="root fuchsia directory, i.e. //",
    )
    parser.add_argument(
        "--build-dir",
        default=environ.get("FUCHSIA_BUILD_DIR"),
        type=str,
        help="build directory to rebase path to, i.e. out/default",
    )
    parser.add_argument(
        "--touch",
        type=Path,
        help="Directory to place timestamp files to mark how up-to-date copy operations are. Provide this option to save work copying files.",
    )
    parser.add_argument(
        "--rust-target-mapping",
        type=Path,
        help="path to json array of rustdoc target info, i.e. out/default/rust_target_mapping.json",
    )
    parser.add_argument(
        "--destination",
        type=Path,
        help="Directory to place merged documentation. Consider using --touch if you run this with a consistent --destination.",
    )
    parser.add_argument(
        "--copy-function",
        default="copy",
        choices=["symlink", "link", "copy"],
    )
    parser.add_argument(
        "--rustdoc-executable",
        help="path directly to the rustdoc executable",
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
        help="run without printing status messages. failure messages still printed",
    )
    parser.add_argument(
        "--build-executable",
        type=Path,
        help="path to fx build program",
    )
    parser.add_argument(
        "--verbose",
        default=False,
        action=BooleanOptionalAction,
        help="add additional context to status messages",
    )
    parser.add_argument(
        "--out-dir",
        help="ignored",
    )
    parser.add_argument(
        "--extra-rustdoc-args",
        nargs="*",
        help="forward extra arguments after -- to rustdoc",
    )
    parser.set_defaults(func=main)
    return parser


if __name__ == "__main__":
    parser = _main_arg_parser()
    args = parser.parse_args(argv[1:])
    args.func(args)
