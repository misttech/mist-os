#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Download some remote artifacts before running a local command.

Usage:
  $0 [options...] -- command...
  $0 [options...] && ...
"""

import argparse
import os
import subprocess

import remote_action

from pathlib import Path
from typing import Iterable, Sequence

_SCRIPT_BASENAME = Path(__file__).name


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def vmsg(verbose: bool, text: str):
    if verbose:
        msg(text)


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Download from stubs and run a local command.",
        argument_default=None,
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Show what is happening",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="Do not download or run the command.",
    )
    parser.add_argument(
        "--download",
        default=[],
        type=Path,
        nargs="*",
        help=
        "Download these files from their stubs.  Arguments are download stub files produced from 'remote_action.py', relative to the working dir.",
    )
    # Positional args are the command and arguments to run.
    parser.add_argument(
        "command", nargs="*", help="The command to run remotely")
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def read_download_stub_infos(
    stub_paths: Iterable[Path],
    verbose: bool = False,
    dry_run: bool = False,
) -> Iterable[remote_action.DownloadStubInfo]:
    for p in stub_paths:
        if remote_action.is_download_stub_file(p):
            vmsg(verbose, f"Read download stub for {p}.")
            yield remote_action.DownloadStubInfo.read_from_file(p)
        else:
            vmsg(verbose, f"{p} is not a download stub, skipping.")


def download_artifacts(
    stub_paths: Iterable[Path],
    exec_root: Path,
    working_dir: Path,
    verbose: bool = False,
    dry_run: bool = False,
) -> int:
    stub_infos = read_download_stub_infos(
        stub_paths, verbose=verbose, dry_run=dry_run)
    # TODO: download in parallel
    download_statuses = [
        (
            stub.path,
            stub.download(exec_root=exec_root, working_dir=working_dir))
        for stub in stub_infos
    ]
    final_status = 0
    for path, status in download_statuses:
        if status != 0:
            final_status = status
            print(f"Error downloading {path}")

    return final_status


def _main(
    argv: Sequence[str],
    exec_root: Path,
    working_dir: Path,
) -> int:
    main_args = _MAIN_ARG_PARSER.parse_args(argv)

    status = download_artifacts(
        stub_paths=main_args.download,
        exec_root=exec_root,
        working_dir=working_dir,
        verbose=main_args.verbose,
        dry_run=main_args.dry_run,
    )
    if status != 0:
        msg("At least one download failed.")
        return status

    if main_args.dry_run:
        msg("Stopping, due to --dry-run")
        return 0

    if main_args.command:
        return subprocess.call(main_args.command)

    return 0


def main(argv: Sequence[str]) -> int:
    return _main(
        argv,
        exec_root=remote_action.PROJECT_ROOT_REL,
        working_dir=Path(os.curdir),
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
