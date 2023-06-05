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
import sys

import cl_utils
import fuchsia
import remote_action
import remotetool

from pathlib import Path
from typing import Iterable, Sequence

_SCRIPT_BASENAME = Path(__file__).name

_PROJECT_ROOT = fuchsia.project_root_dir()

# Needs to be computed with os.path.relpath instead of Path.relative_to
# to support testing a fake (test-only) value of PROJECT_ROOT.
_PROJECT_ROOT_REL = cl_utils.relpath(_PROJECT_ROOT, start=os.curdir)


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
        "--undownload",
        action="store_true",
        default=False,
        help=
        "Restore download stubs, if they exist, and do not run the command.",
    )
    parser.add_argument(
        "--download",
        default=[],
        type=Path,
        nargs="*",
        help=
        "Download these files from their stubs.  Arguments are download stub files produced from 'remote_action.py', relative to the working dir.",
    )
    parser.add_argument(
        "--download_list",
        default=[],
        type=Path,
        nargs="*",
        help=
        "Download these files named in these list files.  Arguments are download stub files produced from 'remote_action.py', relative to the working dir.",
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
    downloader: remotetool.RemoteTool,
    working_dir_abs: Path,
    verbose: bool = False,
    dry_run: bool = False,
) -> int:
    """Download remotely stored artifacts.

    Args:
      stub_paths: path that point to either download stubs or real artifacts.
        Real artifacts are ignored automatically.
    """
    stub_infos = read_download_stub_infos(
        stub_paths, verbose=verbose, dry_run=dry_run)
    # TODO: download in parallel
    download_statuses = [
        (
            stub.path,
            stub.download(
                downloader=downloader,
                working_dir_abs=working_dir_abs,
            )) for stub in stub_infos
    ]
    final_status = 0
    for path, status in download_statuses:
        if status.returncode != 0:
            final_status = status.returncode
            msg(f"Error downloading {path}.  stderr was:")
            for line in status.stderr:
                print(line, file=sys.stderr)

    if final_status != 0:
        msg("At least one download failed.")

    return final_status


def _main(
    argv: Sequence[str],
    downloader: remotetool.RemoteTool,
    working_dir_abs: Path,
) -> int:
    main_args = _MAIN_ARG_PARSER.parse_args(argv)

    paths = main_args.download + list(
        cl_utils.expand_paths_from_files(main_args.download_list))

    if main_args.undownload:
        vmsg(main_args.verbose, f"Restoring download stubs.")
        for p in paths:
            remote_action.undownload(p)

        if main_args.command:
            msg("Not running command, due to --undownload.")
            return 0

        return 0

    # Download artifacts from their stubs.
    status = download_artifacts(
        stub_paths=paths,
        downloader=downloader,
        working_dir_abs=working_dir_abs,
        verbose=main_args.verbose,
        dry_run=main_args.dry_run,
    )

    if status != 0:
        return status

    if main_args.command:
        if main_args.dry_run:
            msg("Stopping, due to --dry-run.")
            return 0

        return subprocess.call(main_args.command)

    return 0


def main(argv: Sequence[str]) -> int:
    with open(_PROJECT_ROOT_REL / remote_action._REPROXY_CFG) as cfg:
        downloader = remotetool.RemoteTool(
            reproxy_cfg=remotetool.read_config_file_lines(cfg))
    return _main(
        argv,
        downloader=downloader,
        working_dir_abs=Path(os.curdir).absolute(),
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
