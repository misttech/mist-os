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
from subprocess import run
from sys import argv, stderr
from tempfile import TemporaryDirectory
from typing import Any, Callable

from rust import HOST_PLATFORM

# fxbug.dev/360942359: This script works, but will not generate cross-crate
# information. The search index, cross-crate trait impls, etc. are broken.
# Functionality using the flags from rust-lang/rfcs#3662 will be added in a
# future patch.


@dataclass
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
    touch: Path
    extern: str
    extern_html_root_url: str
    disable_rustdoc: bool

    def parse(m: dict[str, Any], args: Namespace) -> "Metadata":
        _, crate_name, _ = m["extern"].split("=")
        return Metadata(
            crate_name=crate_name,
            actual_label=m["actual_label"],
            rustdoc_label=m["rustdoc_label"],
            target=m["target"],
            touch=args.build_dir / m["touch"],
            rustdoc_out_dir=args.build_dir / m["rustdoc_out_dir"],
            extern=m["extern"],
            extern_html_root_url=m["extern_html_root_url"],
            disable_rustdoc=m["disable_rustdoc"],
        )


def read_metadata_file(args: Namespace) -> list[Metadata]:
    """reads rust_target_mapping.json to get metadata"""
    meta = args.rust_target_mapping.read_bytes()
    meta = json.loads(meta)
    return [Metadata.parse(m, args) for m in meta]


def fx_build_all(meta: list[Metadata], args: Namespace) -> None:
    """
    Build docs for all rustdoc targets, including
    third_party crates. These may not have been documented
    during a normal `fx build` run.

    Raises:
        CalledProcessError if the the executable --build-executable runs fails.
    """
    print("Documenting individual crates", file=stderr)
    rustdoc_labels = (m.rustdoc_label for m in meta if not m.disable_rustdoc)
    # build `actual_label`s because we link against their generated rmetas
    # in this script's rustdoc invocation.
    actual_labels = (m.actual_label for m in meta if not m.disable_rustdoc)
    labels_to_build = [*rustdoc_labels, *actual_labels]
    # if `fx build` has no arguments, it will build everything. Avoid that.
    if len(labels_to_build) > 0:
        completed = run([args.build_executable, *labels_to_build])
        completed.check_returncode()
    else:
        print(
            f"did not find any !disable_rustdoc targets to build", file=stderr
        )


def group_by(items: list, group: Callable) -> dict[str, list]:
    grouped = defaultdict(list)
    for m in items:
        grouped[group(m)].append(m)
    return grouped


def dedup_crate_name(meta: list[Metadata]) -> list[Metadata]:
    """remove metadata for duplicate crate names"""
    ret = dict()  # str -> Metadata
    for m in meta:
        if m.crate_name in ret:
            print(
                f"found two crates with name {m.crate_name}:",
                f"{m.actual_label}, and {ret[m.crate_name].actual_label}",
                file=stderr,
            )
            continue
        ret[m.crate_name] = m
    return list(ret.values())


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

    extern_html_root_urls = set(
        f"--extern-html-root-url={m.crate_name}={m.extern_html_root_url}"
        for m in meta
    )

    rust_searchdir = args.rust_searchdir.read_bytes()
    rust_searchdir = set(json.loads(rust_searchdir))

    for target, meta in group_by(meta, lambda m: m.target).items():
        meta = dedup_crate_name(meta)
        meta = [m for m in meta if not m.disable_rustdoc]

        extra_rustdoc_args = (
            args.extra_rustdoc_args
            if args.extra_rustdoc_args is not None
            else []
        )

        dst = args.destination / target
        extern = set(m.extern for m in meta)

        print("running rustdoc to document index for", target, file=stderr)
        with TemporaryDirectory() as tmp:
            crate_root = Path(tmp, "lib.rs")
            crate_names = [f"extern crate {m.crate_name};" for m in meta]
            crate_root.write_text("\n".join(crate_names))
            argfile = Path(tmp, "argfile")
            flags = [
                str(crate_root),
                *extern,
                *rust_searchdir,
                *extern_html_root_urls,
                "--crate-name=fuchsia_rustdoc_index",
                "--crate-type=lib",
                "--edition=2021",
                f"--target={target}",
                "-Zunstable-options",
                f"--out-dir={dst}",
                "--enable-index-page",
                *extra_rustdoc_args,
            ]
            argfile.write_text("\n".join(flags))
            completed = run(
                [args.rustdoc_executable, f"@{argfile}"], cwd=args.build_dir
            )
            completed.check_returncode()

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
    meta = read_metadata_file(args)
    if args.build:
        fx_build_all(meta, args)
    rustdoc_link(meta, args)


def environ_path_join(key: str, *rest) -> Path:
    """
    Returns a path with value of key (an environment variable) with rest.
    Asserts that the value for key exists in the environment.
    """
    value = environ.get(key)
    assert (
        value is not None
    ), f"expected the variable {key} to be in the environment"
    return Path(value, *rest)


def _main_arg_parser() -> ArgumentParser:
    parser = ArgumentParser(
        description="Documents all (subject to `build_only_labels`) rust crates, and merges the docs to a single destination."
    )
    parser.add_argument(
        "--rust-searchdir",
        default=environ_path_join("FUCHSIA_BUILD_DIR", "rust_searchdir.json"),
        type=Path,
        help="path to json array of searchdir -Ldependency arguments, i.e. out/default/rust_searchdir.json",
    )
    parser.add_argument(
        "--touch",
        default=environ_path_join("FUCHSIA_BUILD_DIR", "docs/rust/touch"),
        type=Path,
        help="Directory to place timestamp files to mark how up-to-date copy operations are. Provide this option to save work copying files.",
    )
    parser.add_argument(
        "--rust-target-mapping",
        type=Path,
        default=environ_path_join(
            "FUCHSIA_BUILD_DIR", "rust_target_mapping.json"
        ),
        help="path to json array of rustdoc target info, i.e. out/default/rust_target_mapping.json",
    )
    parser.add_argument(
        "--build-dir",
        default=environ_path_join("FUCHSIA_BUILD_DIR"),
        type=Path,
        help="build directory to rebase path to, i.e. out/default",
    )
    parser.add_argument(
        "--destination",
        default=environ_path_join("FUCHSIA_BUILD_DIR", "docs/rust/doc"),
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
        default=environ_path_join(
            "FUCHSIA_DIR",
            "prebuilt/third_party/rust",
            HOST_PLATFORM,
            "bin/rustdoc",
        ),
        help="path directly to the rustdoc executable",
    )
    parser.add_argument(
        "--build",
        default=True,
        action=BooleanOptionalAction,
        help="run fx build to build extra rustdoc targets. --no-build will break this script unless the third party rust targets are already manually built",
    )
    parser.add_argument(
        "--build-executable",
        default=environ_path_join("FUCHSIA_DIR", "tools/devshell/build"),
        type=Path,
        help="path to fx build program",
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
