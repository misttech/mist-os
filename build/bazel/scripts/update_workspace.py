#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Update or generate Bazel workspace for the platform build.

The script first checks whether the Ninja build plan needs to be updated.
After that, it checks whether the Bazel workspace used by the platform
build, and associated files (e.g. bazel launcher script) and
directories (e.g. output_base) need to be updated. It simply exits
if no update is needed, otherwise, it will regenerate everything
appropriately.

The TOPDIR directory argument will be populated with the following
files:

  $TOPDIR/
    bazel                   Bazel launcher script.
    generated-info.json     State of inputs during last generation.
    output_base/            Bazel output base.
    output_user_root/       Bazel output user root.
    workspace/              Bazel workspace directory.

The workspace/ sub-directory will be populated with symlinks
mirroring the top FUCHSIA_DIR directory, except for the 'out'
sub-directory and a few other files. It will also include a few
generated files (e.g. `.bazelrc`).

The script tracks the file and sub-directory entries of $FUCHSIA_DIR,
and is capable of updating the workspace if new ones are added, or
old ones are removed.
"""

import argparse
import difflib
import json
import os
import stat
import sys
from pathlib import Path

import check_ninja_build_plan
import compute_content_hash
import workspace_utils


def make_removable(path: str) -> None:
    """Ensure the file at |path| is removable."""

    islink = os.path.islink(path)

    # Skip if the input path is a symlink, and chmod with
    # `follow_symlinks=False` is not supported. Linux is the most notable
    # platform that meets this requirement, and adding S_IWUSR is not necessary
    # for removing symlinks.
    if islink and (os.chmod not in os.supports_follow_symlinks):
        return

    info = os.stat(path, follow_symlinks=False)
    if info.st_mode & stat.S_IWUSR == 0:
        try:
            if islink:
                os.chmod(
                    path, info.st_mode | stat.S_IWUSR, follow_symlinks=False
                )
            else:
                os.chmod(path, info.st_mode | stat.S_IWUSR)
        except Exception as e:
            raise RuntimeError(
                f"Failed to chmod +w to {path}, islink: {islink}, info: {info}, error: {e}"
            )


def remove_dir(path: str) -> None:
    """Properly remove a directory."""
    # shutil.rmtree() does not work well when there are readonly symlinks to
    # directories. This results in weird NotADirectory error when trying to
    # call os.scandir() or os.rmdir() on them (which happens internally).
    #
    # Re-implement it correctly here. This is not as secure as it could
    # (see "shutil.rmtree symlink attack"), but is sufficient for the Fuchsia
    # build.
    all_files = []
    all_dirs = []
    for root, subdirs, files in os.walk(path):
        # subdirs may contain actual symlinks which should be treated as
        # files here.
        real_subdirs = []
        for subdir in subdirs:
            if os.path.islink(os.path.join(root, subdir)):
                files.append(subdir)
            else:
                real_subdirs.append(subdir)

        for file in files:
            file_path = os.path.join(root, file)
            all_files.append(file_path)
            make_removable(file_path)
        for subdir in real_subdirs:
            dir_path = os.path.join(root, subdir)
            all_dirs.append(dir_path)
            make_removable(dir_path)

    for file in reversed(all_files):
        os.remove(file)
    for dir in reversed(all_dirs):
        os.rmdir(dir)
    os.rmdir(path)


def create_clean_dir(path: str) -> None:
    """Create a clean directory."""
    if os.path.exists(path):
        remove_dir(path)
    os.makedirs(path)


def get_fx_build_dir(fuchsia_dir: str) -> str | None:
    """Return the path to the Ninja build directory set through 'fx set'.

    Args:
      fuchsia_dir: The Fuchsia top-level source directory.
    Returns:
      The path to the Ninja build directory, or None if it could not be
      determined, which happens on infra bots which do not use 'fx set'
      but 'fint set' directly.
    """
    fx_build_dir_path = os.path.join(fuchsia_dir, ".fx-build-dir")
    if not os.path.exists(fx_build_dir_path):
        return None

    with open(fx_build_dir_path) as f:
        build_dir = f.read().strip()
        return os.path.join(fuchsia_dir, build_dir)


def content_hash_file(path: str | Path) -> str:
    fstate = compute_content_hash.FileState()
    fstate.hash_source_path(Path(path))
    return fstate.content_hash


def find_host_binary_path(program: str) -> str:
    """Find the absolute path of a given program. Like the UNIX `which` command.

    Args:
        program: Program name.
    Returns:
        program's absolute path, found by parsing the content of $PATH.
        An empty string is returned if nothing is found.
    """
    for path in os.environ.get("PATH", "").split(":"):
        # According to Posix, an empty path component is equivalent to ´.'.
        if path == "" or path == ".":
            path = os.getcwd()
        candidate = os.path.realpath(os.path.join(path, program))
        if os.path.isfile(candidate) and os.access(
            candidate, os.R_OK | os.X_OK
        ):
            return candidate

    return ""


_VALID_TARGET_CPUS = ("arm64", "x64", "riscv64")


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "--fuchsia-dir",
        help="Path to the Fuchsia source tree, auto-detected by default.",
    )
    parser.add_argument(
        "--gn_output_dir", help="GN output directory, auto-detected by default."
    )
    parser.add_argument(
        "--target_arch",
        help="Equivalent to `target_cpu` in GN. Defaults to args.gn setting.",
        choices=_VALID_TARGET_CPUS,
    )
    parser.add_argument(
        "--bazel-bin",
        help="Path to bazel binary, defaults to $FUCHSIA_DIR/prebuilt/third_party/bazel/${host_platform}/bazel",
    )
    parser.add_argument(
        "--topdir",
        help="Top output directory. Defaults to GN_OUTPUT_DIR/gen/build/bazel",
    )
    parser.add_argument(
        "--use-bzlmod",
        action="store_true",
        help="Use BzlMod to generate external repositories.",
    )
    parser.add_argument(
        "--verbose", action="count", default=1, help="Increase verbosity"
    )
    parser.add_argument(
        "--quiet", action="count", default=0, help="Reduce verbosity"
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Force workspace regeneration, by default this only happens "
        + "the script determines there is a need for it.",
    )
    parser.add_argument(
        "--depfile",
        type=argparse.FileType("w"),
        help="If set, write a depfile at this path",
    )
    args = parser.parse_args()

    verbosity = args.verbose - args.quiet

    if not args.fuchsia_dir:
        # Assume this script is in 'build/bazel/scripts/'
        # //build/bazel:generate_main_workspace always sets this argument,
        # this feature is a convenience when calling this script manually
        # during platform build development and debugging.
        args.fuchsia_dir = os.path.join(
            os.path.dirname(__file__), "..", "..", ".."
        )

    fuchsia_dir = os.path.abspath(args.fuchsia_dir)

    if not args.gn_output_dir:
        args.gn_output_dir = get_fx_build_dir(fuchsia_dir)
        if not args.gn_output_dir:
            parser.error(
                "Could not auto-detect build directory, please use --gn_output_dir=DIR"
            )
            return 1

    gn_output_dir = os.path.abspath(args.gn_output_dir)

    if not args.target_arch:
        # Extract default target architecture from args.json file, which is
        # created after calling `gn gen`. If the file is missing print an error
        args_json_path = os.path.join(args.gn_output_dir, "args.json")
        if not os.path.exists(args_json_path):
            parser.error(
                f"Cannot determine target architecture ({args_json_path} is missing). Please use --target-arch=ARCH"
            )
        with open(args_json_path) as f:
            args_json = json.load(f)
            target_cpu = args_json.get("target_cpu", None)
            if target_cpu not in _VALID_TARGET_CPUS:
                parser.error(
                    f'Unsupported target cpu value "{target_cpu}" from {args_json_path}'
                )
            args.target_arch = target_cpu

    if not args.topdir:
        default_topdir = workspace_utils.get_bazel_relative_topdir(
            fuchsia_dir, "main"
        )
        args.topdir = os.path.join(gn_output_dir, default_topdir)

    topdir = os.path.abspath(args.topdir)

    logs_dir = os.path.join(topdir, "logs")

    host_tag = workspace_utils.get_host_tag()

    ninja_binary = os.path.join(
        fuchsia_dir, "prebuilt", "third_party", "ninja", host_tag, "ninja"
    )

    python_prebuilt_dir = os.path.join(
        fuchsia_dir, "prebuilt", "third_party", "python3", host_tag
    )

    output_base_dir = os.path.abspath(os.path.join(topdir, "output_base"))
    output_user_root = os.path.abspath(os.path.join(topdir, "output_user_root"))
    workspace_dir = os.path.abspath(os.path.join(topdir, "workspace"))

    if not args.bazel_bin:
        args.bazel_bin = os.path.join(
            fuchsia_dir, "prebuilt", "third_party", "bazel", host_tag, "bazel"
        )

    bazel_bin = os.path.abspath(args.bazel_bin)

    bazel_launcher = os.path.abspath(os.path.join(topdir, "bazel"))

    def log(message: str, level: int = 1) -> None:
        if verbosity >= level:
            print(message)

    def log2(message: str) -> None:
        log(message, 2)

    log2(
        f"""Using directories and files:
  Fuchsia:                {fuchsia_dir}
  GN build:               {gn_output_dir}
  Ninja binary:           {ninja_binary}
  Bazel source:           {bazel_bin}
  Topdir:                 {topdir}
  Logs directory:         {logs_dir}
  Bazel workspace:        {workspace_dir}
  Bazel output_base:      {output_base_dir}
  Bazel output user root: {output_user_root}
  Bazel launcher:         {bazel_launcher}
"""
    )

    if not check_ninja_build_plan.ninja_plan_is_up_to_date(gn_output_dir):
        print(
            f"Ninja build plan in {gn_output_dir} is not up-to-date, please run `fx build` before this script!",
            file=sys.stderr,
        )
        return 1

    log2("Ninja build plan up to date.")

    generated = workspace_utils.GeneratedWorkspaceFiles()
    generated.set_file_hasher(content_hash_file)

    # Ensure regeneration when this script's content changes!
    generated.record_input_file_hash(os.path.abspath(__file__))

    workspace_utils_py = os.path.join(
        os.path.dirname(__file__), "workspace_utils.py"
    )
    generated.record_input_file_hash(os.path.abspath(workspace_utils_py))

    git_bin_path = find_host_binary_path("git")
    assert git_bin_path, "Missing `git` program in current PATH!"

    workspace_utils.record_fuchsia_workspace(
        generated,
        top_dir=Path(topdir),
        fuchsia_dir=Path(fuchsia_dir),
        gn_output_dir=Path(gn_output_dir),
        git_bin_path=Path(git_bin_path),
        target_cpu=args.target_arch,
        enable_bzlmod=bool(args.use_bzlmod),
    )

    force = args.force
    generated_json = generated.to_json()
    generated_info_file = os.path.join(topdir, "generated-info.json")
    if not os.path.exists(generated_info_file):
        log2("Missing file: " + generated_info_file)
        force = True
    elif not os.path.isdir(workspace_dir):
        log2("Missing directory: " + workspace_dir)
        force = True
    elif not os.path.isdir(output_base_dir):
        log2("Missing directory: " + output_base_dir)
        force = True
    else:
        with open(generated_info_file) as f:
            existing_info = f.read()

        if existing_info != generated_json:
            log2("Changes in %s" % (generated_info_file))
            if verbosity >= 2:
                print(
                    "\n".join(
                        difflib.unified_diff(
                            existing_info.splitlines(),
                            generated_json.splitlines(),
                        )
                    )
                )
            force = True

    if force:
        log(
            "Regenerating Bazel workspace%s."
            % (", --force used" if args.force else "")
        )

        # Remove the old workspace's content, which is just a set
        # of symlinks and some auto-generated files.
        create_clean_dir(workspace_dir)

        # Do not clean the output_base because it is now massive,
        # and doing this will very slow and will force a lot of
        # unnecessary rebuilds after that,
        #
        # However, do remove $OUTPUT_BASE/external/ which holds
        # the content of external repositories.
        #
        # This is a guard against repository rules that do not
        # list their input dependencies properly, and would fail
        # to re-run if one of them is updated. Sadly, this is
        # pretty common with Bazel.
        create_clean_dir(os.path.join(output_base_dir, "external"))

        # Repopulate the workspace directory.
        generated.write(topdir)

        # Update the content of generated-info.json file.
        with open(generated_info_file, "w") as f:
            f.write(generated_json)
    else:
        log2("Nothing to do (no changes detected)")

    # Done!
    return 0


if __name__ == "__main__":
    sys.exit(main())
