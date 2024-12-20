#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Regenerate the Ninja build plan and the Bazel workspace for the Fuchsia build system."""

import argparse
import os
import shlex
import subprocess
import sys
import typing as T
from pathlib import Path

_SCRIPT_DIR = Path(__file__).parent

# Assume this script lives under //build/
_FUCHSIA_DIR = _SCRIPT_DIR.parent.resolve()

_DEFAULT_HOST_TAG = "linux-x64"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fuchsia-dir",
        type=Path,
        default=_FUCHSIA_DIR,
        help="Fuchsia source directory (auto-detected).",
    )
    parser.add_argument(
        "--fuchsia-build-dir",
        type=Path,
        help="Fuchsia build directory (auto-detected).",
    )
    parser.add_argument(
        "--host-tag",
        default=_DEFAULT_HOST_TAG,
        help=f"Fuchsia host tag (default {_DEFAULT_HOST_TAG})",
    )
    parser.add_argument(
        "--update-args",
        action="store_true",
        help="Update args.gn before regeneration.",
    )
    parser.add_argument(
        "--symlinks-only",
        action="store_true",
        help="Only create convenience symlinks, do not regenerate build plan.",
    )
    parser.add_argument(
        "--no-export-rust-project",
        action="store_true",
        help="Disable generation of the rust-project.json file.",
    )
    parser.add_argument(
        "--color", action="store_true", help="Force colored output."
    )
    parser.add_argument(
        "--gn-tracelog", type=Path, help="Path to GN --tracelog output file."
    )
    parser.add_argument(
        "--gn-ide",
        action="append",
        default=[],
        help="Pass value to GN --ide option.",
    )
    parser.add_argument(
        "--gn-json-ide-script",
        action="append",
        default=[],
        help="Pass value to GN --json-ide-script option.",
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="count",
        default=0,
        help="Increase verbosity.",
    )
    args = parser.parse_args()

    verbose = args.verbose

    def log(msg: str) -> None:
        if verbose >= 1:
            print(msg)

    def log2(msg: str) -> None:
        if verbose >= 2:
            print(msg)

    def run_cmd(
        args: T.Sequence[str], **kwd: T.Any
    ) -> "subprocess.CompletedProcess[str]":
        if verbose >= 2:
            log("CMD: %s" % " ".join(shlex.quote(str(a)) for a in args))
        return subprocess.run(args, text=True, **kwd)

    if not args.fuchsia_build_dir:
        dot_fx_build_dir = args.fuchsia_dir / ".fx-build-dir"
        if not dot_fx_build_dir.exists():
            parser.error(
                "Cannot auto-detect build directory, please use --fuchsia-build-dir=DIR option!"
            )
        args.fuchsia_build_dir = (
            args.fuchsia_dir / dot_fx_build_dir.read_text().strip()
        )

    fuchsia_dir = args.fuchsia_dir.resolve()
    build_dir = args.fuchsia_build_dir.resolve()
    if not build_dir.is_dir():
        parser.error(f"Build directory missing or invalid: {build_dir}")

    if not args.symlinks_only:
        # Run `gn gen` in fuchsia_dir to regenerate the Ninja build plan.
        prebuilt_gn_subpath = f"prebuilt/third_party/gn/{args.host_tag}/gn"
        prebuilt_ninja_subpath = (
            f"prebuilt/third_party/ninja/{args.host_tag}/ninja"
        )

        build_dir_to_source_dir = os.path.relpath(fuchsia_dir, build_dir)

        gn_cmd_args = [
            prebuilt_gn_subpath,
            "args" if args.update_args else "gen",
            os.path.relpath(build_dir, fuchsia_dir),
            "--fail-on-unused-args",
            "--check=system",
            f"--ninja-executable={fuchsia_dir}/{prebuilt_ninja_subpath}",
            "--ninja-outputs-file=ninja_outputs.json",
        ]
        if not args.no_export_rust_project:
            gn_cmd_args += ["--export-rust-project"]

        if args.color:
            gn_cmd_args += ["--color"]

        if args.gn_tracelog:
            gn_cmd_args += [f"--tracelog={args.gn_tracelog}"]

        for gn_ide in args.gn_ide:
            gn_cmd_args += [f"--ide={gn_ide}"]

        for gn_json_ide_script in args.gn_json_ide_script:
            gn_cmd_args += [f"--json-ide-script={gn_json_ide_script}"]

        log("Running gn gen to rebuild Ninja manifest...")
        ret = run_cmd(gn_cmd_args, cwd=args.fuchsia_dir)
        if ret.returncode != 0:
            # Don't print anything here, assume GN already wrote something to the user.
            return ret.returncode

        log(
            "Patching Ninja build plan to invoke regenerator on build file changes."
        )

        # Patch build.ninja to ensure Ninja calls this script instead of `gn gen`
        # if any of the BUILD file changes.
        log2("- Patching build.ninja")
        build_ninja_path = build_dir / "build.ninja"
        build_ninja = build_ninja_path.read_text()
        pos = build_ninja.find("rule gn\n  command = ")
        if pos < 0:
            print(
                'ERROR: Cannot find "gn" rule in Ninja manifest!',
                file=sys.stderr,
            )
            return 1

        regenerator_command_args = [
            sys.executable,
            "-S",
            f"{build_dir_to_source_dir}/build/regenerator.py",
            f"--fuchsia-dir={shlex.quote(str(fuchsia_dir))}",
            f"--fuchsia-build-dir={shlex.quote(str(build_dir))}",
            f"--host-tag={args.host_tag}",
        ]
        regenerator_command = " ".join(
            shlex.quote(str(a)) for a in regenerator_command_args
        )

        pos2 = build_ninja.find("\n", pos + 16)
        assert pos2 >= 0
        build_ninja = (
            build_ninja[:pos]
            + f"rule gn\n  command = {regenerator_command}"
            + build_ninja[pos2:]
        )
        build_ninja_path.write_text(build_ninja)

        # Patch build.ninja.d to ensure that Ninja will re-invoke the script if its
        # own source code changes.
        log2("- Patching build.ninja.d")
        build_ninja_d_path = build_dir / "build.ninja.d"
        build_ninja_d = build_ninja_d_path.read_text().rstrip()
        build_ninja_d += f" {build_dir_to_source_dir}/build/regenerator {build_dir_to_source_dir}/build/regenerator.py"
        build_ninja_d_path.write_text(build_ninja_d)

        log2("- Updating build.ninja.stamp timestamp")
        build_ninja_stamp_path = build_dir / "build.ninja.stamp"
        build_ninja_stamp_path.touch()

    # Create convenience symlinks in fuchsia_dir, these should only be used by
    # developers manually or by IDEs, but the build should never depend on them.
    def make_relative_symlink(link_path: Path, target_path: Path) -> None:
        if link_path.is_symlink() or link_path.exists():
            link_path.unlink()
        relative_target = os.path.relpath(
            target_path.resolve(), link_path.parent.resolve()
        )
        log2(f"- Symlink {link_path} --> {relative_target}")
        link_path.symlink_to(relative_target)

    log("Creating convenience symlinks.")

    # These artifacts need to be in the source checkout.
    # NOTE: Nothing should depend on this in the build, as some developers
    # need to run multiple builds concurrently from the same source checkout
    # but using different build directories.

    # These symlinks are useful for IDEs.
    for artifact in ("compile_commands.json", "rust-project.json"):
        if (build_dir / artifact).exists():
            make_relative_symlink(fuchsia_dir / artifact, build_dir / artifact)

    # These symlink help debug Bazel issues. See //build/bazel/README.md
    make_relative_symlink(
        fuchsia_dir / "bazel-workspace",
        build_dir / "gen" / "build" / "bazel" / "workspace",
    )

    make_relative_symlink(
        fuchsia_dir / "bazel-bin",
        build_dir / "gen" / "build" / "bazel" / "workspace" / "bazel-bin",
    )

    make_relative_symlink(
        fuchsia_dir / "bazel-out",
        build_dir / "gen" / "build" / "bazel" / "workspace" / "bazel-out",
    )

    make_relative_symlink(
        fuchsia_dir / "bazel-repos",
        build_dir / "gen" / "build" / "bazel" / "output_base" / "external",
    )

    log("Done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
