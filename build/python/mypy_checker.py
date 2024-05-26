#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
""" This script checks python type checking for the input sources.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import package_python_binary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target_name",
        help="Name of the build target",
        required=True,
    )
    parser.add_argument(
        "--library_infos",
        help="Path to the library infos JSON file",
        type=argparse.FileType("r"),
        required=True,
    )
    parser.add_argument(
        "--gen_dir",
        help="Path to gen directory, used to stage temporary directories",
        type=Path,
        required=True,
    )
    parser.add_argument(
        "--depfile",
        help="Path to the depfile to generate",
        type=Path,
        required=True,
    )
    parser.add_argument(
        "--output",
        help="Path to output file",
    )

    args = parser.parse_args()

    # To satisfy GN action requirements, creating a necessary empty output file
    Path(args.output).touch()

    lib_infos = json.load(args.library_infos)
    return run_mypy_on_library_target(
        args.target_name,
        args.gen_dir,
        lib_infos,
        args.depfile,
    )


def run_mypy_on_binary_target(
    target_name: str,
    gen_dir: str,
    src_files: list[str],
    data_sources: list[str],
    data_package_name: str | None,
    lib_infos: list[dict[str, object]],
) -> int:
    """
    Runs `mypy` type checking on the sources of a binary target, as well as the
    sources of all its library dependencies that have "mypy_enable = True".

    Args:
        target_name: The name of the target being built.
        gen_dir: The path to the generated directory
        src_files: The list of sources of the binary target.
        data_sources: The list of data source file paths.
        data_package_name: Name of the Python package to store data sources in.
        lib_infos: The list of library dependencies of the binary target

    Returns:
        The exit code of the Mypy invocation.
    """
    # Temporary directory to stage the source tree for Mypy checks,
    # including sources of itself and all the libraries it imports.
    tmp_dir: str = os.path.join(gen_dir, target_name + "_bin_mypy")
    os.makedirs(tmp_dir, exist_ok=True)

    # Mapping from the temp dir source file paths to the original source file paths
    src_map: dict[str, str] = {}

    # Copy over the sources of this target to the tmp directory.
    ret = package_python_binary.copy_binary_sources(
        dest_dir=tmp_dir,
        sources=src_files,
        data_sources=data_sources,
        data_package_name=data_package_name,
        src_map=src_map,
    )
    if ret != 0:
        return ret

    # Copy mypy enabled library sources to the tmp directory.
    package_python_binary.copy_library_sources(
        tmp_dir, [info for info in lib_infos if info["mypy_support"]], src_map
    )
    try:
        ret = run_mypy_checks(target_name, tmp_dir, src_map)
    finally:
        package_python_binary.remove_dir(tmp_dir)
    return ret


def run_mypy_on_library_target(
    target_name: str,
    gen_dir: Path,
    lib_infos: list[dict[str, object]],
    depfile: Path,
) -> int:
    """
    Runs `mypy` type checking on the sources of a library target, as well as the
    sources of all its library dependencies that have "mypy_enable = True".

    Args:
        target_name: The name of the target being built.
        gen_dir: The path to the generated directory
        lib_infos: The list of library dependencies of the library target
        depfile: The path to the depfile to generate

    Returns:
        The exit code of the Mypy invocation.
    """
    # Temporary directory to stage the source tree for Mypy checks,
    # including sources of itself and all the libraries it imports.
    tmp_dir = gen_dir / f"{target_name}_lib_mypy"
    os.makedirs(tmp_dir, exist_ok=True)

    # Mapping from the temp dir source file paths to the original source file paths
    src_map: dict[str, str] = {}

    # Copy mypy enabled library sources to the tmp directory.
    package_python_binary.copy_library_sources(
        tmp_dir, [info for info in lib_infos if info["mypy_support"]], src_map
    )

    # Write the depfile
    depfile.write_text(
        "{}: {}\n".format(tmp_dir, " ".join(list(src_map.values())))
    )

    try:
        ret = run_mypy_checks(target_name, str(tmp_dir), src_map)
    finally:
        package_python_binary.remove_dir(tmp_dir)
    return ret


def run_mypy_checks(
    target_name: str,
    app_dir: str,
    src_map: dict[str, str],
) -> int:
    """
    Runs `mypy` type checking on the app_dir, refactors the output to reflect
    the original file paths.

    Args:
        target_name: The name of the target being built
        app_dir: The path of the directory to run type checking on
        src_map: Mapping from the original source file paths to app dir paths

    Returns:
        int: returncode if type checking was successful, else error returncode
    """
    fuchsia_dir = Path(__file__).parent.parent.parent
    config_path = fuchsia_dir / "pyproject.toml"
    pylibs_dir = fuchsia_dir / "third_party" / "pylibs"
    try:
        return subprocess.run(
            [
                sys.executable,
                "-S",
                "-m",
                "mypy",
                "--config-file",
                str(config_path),
                app_dir,
            ],
            env={
                "PYTHONPATH": os.pathsep.join(
                    [
                        str(pylibs_dir / "mypy" / "src"),
                        str(pylibs_dir / "mypy_extensions" / "src"),
                        str(pylibs_dir / "typing_extensions" / "src" / "src"),
                    ]
                )
            },
            capture_output=True,
            text=True,
            check=True,
        ).returncode
    except subprocess.CalledProcessError as e:
        if e.returncode != 0:
            if e.stdout:
                refactored_out = convert_mypy_output(e.stdout, src_map)
                print(
                    f"\nPlease fix the following Mypy errors:\n{refactored_out}\n",
                    file=sys.stderr,
                )
            else:
                print(
                    f"\nError occured during Mypy type checking:\n{e.stderr}\n",
                    file=sys.stderr,
                )
        return e.returncode


def convert_mypy_output(output: str, src_map: dict[str, str]) -> str:
    """
    Converts the mypy output to use the source file paths as specified in
    src_map instead of the temp directory paths.

    Args:
        output: The mypy output to refactor
        src_map: A dictionary mapping from the original source file paths to the
            corresponding temp directory source file paths

    Returns:
        A refactored version of mypy output with the original source file paths
    """
    output_lines = output.splitlines()

    # e.g. "src/tests/end_to_end/rtc/test_rtc_conformance.py:12: error:"
    source_matcher = re.compile(r"^(\S+\.py):\d+")

    for i, line in enumerate(output_lines):
        match = source_matcher.search(line)
        if match:
            source = match.group(1)
            output_lines[i] = line.replace(
                source, src_map[source].replace("../../", "")
            )

    return "\n".join(output_lines)


if __name__ == "__main__":
    sys.exit(main())
