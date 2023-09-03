#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""bb_fetch_rbe_cas

Given a bbid and the path of an intermediate output that was remote-built
using RBE, fetch the blob from the RBE CAS.
Note: permissions are not set, so the caller might need to set executable bits.
"""

import argparse
import json
import os
import sys
import tempfile

import cl_utils
import fuchsia
import remotetool
import reproxy_logs
# This requires python pb2 in build/rbe/proto (generated).

from pathlib import Path
from typing import Any, Dict, Optional, Sequence, Tuple

_SCRIPT_BASENAME = Path(__file__).name
_SCRIPT_DIR = Path(__file__).parent

PROJECT_ROOT = fuchsia.project_root_dir()
PROJECT_ROOT_REL = cl_utils.relpath(PROJECT_ROOT, start=os.curdir)

_REPROXY_CFG = _SCRIPT_DIR / "fuchsia-reproxy.cfg"

# default path to `bb` buildbucket tool
_BB_TOOL = PROJECT_ROOT_REL / 'prebuilt' / 'tools' / 'buildbucket' / 'bb'


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Fetch build artifacts from RBE CAS.",
        argument_default=None,
    )
    parser.add_argument(
        "--bb",
        type=Path,
        default=_BB_TOOL,
        help="Path to 'bb' CLI tool.",
        metavar='PATH',
    )
    parser.add_argument(
        "--cfg",
        type=Path,
        default=_REPROXY_CFG,
        help="Reproxy configuration file.",
        metavar='FILE',
    )

    # Require one of the following:
    input_group = parser.add_mutually_exclusive_group(required=True)
    input_group.add_argument(
        "--bbid",
        type=str,
        help=
        "Buildbucket ID (leading 'b' optional/permitted).  Begin search fore reproxy log here.",
        metavar="ID",
    )
    input_group.add_argument(
        "--reproxy_log",
        type=Path,
        help="Use this reproxy log directly instead of searching from bbid.",
        metavar="LOG",
    )

    parser.add_argument(
        "--path",
        type=Path,
        help=
        "Path of artifact under the build output directory.  If omitted, print the path to the cached reproxy log and exit without downloading anything.",
        default=None,
    )
    parser.add_argument(
        "-o",
        dest="output",
        type=Path,
        default=None,
        help=
        "Local output location to fetch to.  If omitted, use the basename of the --path argument.",
        metavar="PATH",
    )
    parser.add_argument(
        "--verbose",
        default=False,
        action=argparse.BooleanOptionalAction,
        help="Print step details.",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


class BBError(RuntimeError):
    """BuildBucketTool related errors."""

    def __init__(self, msg: str):
        super().__init__(msg)


class BuildBucketTool(object):

    def __init__(self, bb: Path = None):
        self.bb = bb or _BB_TOOL

    def get_json_fields(self, bbid: str) -> Dict[str, Any]:
        bb_result = cl_utils.subprocess_call(
            [
                str(self.bb), 'get', bbid, '--json', '--fields',
                'output.properties'
            ],
            quiet=True)
        if bb_result.returncode != 0:
            for line in bb_result.stderr:
                print(line)
            raise BBError(f'bb failed to lookup id {bbid}.')
        return json.loads('\n'.join(bb_result.stdout) + '\n')

    def download_reproxy_log(self, build_id: str, reproxy_log_name: str) -> str:
        # 'bb log' prints log contents to stdout.  Capture it and write it out.
        rpl_log_result = cl_utils.subprocess_call(
            [
                str(self.bb), 'log', build_id,
                f'build|teardown remote execution|read {reproxy_log_name}',
                reproxy_log_name
            ],
            quiet=True)
        if rpl_log_result.returncode != 0:
            for line in rpl_log_result.stderr:
                print(line)
            raise BBError(f'Failed to fetch bb reproxy log {reproxy_log_name}.')
        return '\n'.join(rpl_log_result.stdout) + '\n'

    def fetch_reproxy_log_cached(
            self,
            build_id: str,
            reproxy_log_name: str,
            verbose: bool = False) -> Path:
        tempdir = Path(tempfile.gettempdir())
        reproxy_log_cache_path = tempdir / _SCRIPT_BASENAME / 'reproxy_logs_cache' / f'b{build_id}' / reproxy_log_name
        reproxy_log_cache_path.parent.mkdir(parents=True, exist_ok=True)
        if reproxy_log_cache_path.exists():
            if verbose:
                msg(f"Re-using cached reproxy log at {reproxy_log_cache_path}")
        else:
            if verbose:
                msg(
                    f"Downloading reproxy log {reproxy_log_name} from buildbucket.  (This could take a few minutes.)"
                )
            rpl_log_contents = self.download_reproxy_log(
                build_id, reproxy_log_name)
            reproxy_log_cache_path.write_text(rpl_log_contents)
            if verbose:
                msg(f"Reproxy log cached to {reproxy_log_cache_path}")

        return reproxy_log_cache_path

    def get_rbe_build_info(self,
                           bbid: str,
                           verbose: bool = False) -> Tuple[str, Dict[str, Any]]:
        """Returns info for the build that actually used RBE (maybe a subbuild).

        Args:
          bbid: build id
          verbose: if true, print what is happening

        Returns:
          1) build id
          2) output properties as JSON object
        """
        if verbose:
            msg(f'Looking up output properties of build {bbid}')
        bb_json = self.get_json_fields(bbid)

        if verbose:
            msg(f'Checking for child build of {bbid}')

        output_properties = bb_json['output']['properties']
        child_build_id = output_properties.get('child_build_id')

        # `rbe_build_id` points to the build that contains
        # the reproxy log with information about a remote built
        # artifact.
        if child_build_id:
            rbe_build_id = child_build_id
            rbe_build_json = self.get_json_fields(rbe_build_id)
        else:
            # re-use bb_json
            rbe_build_id = bbid
            rbe_build_json = bb_json

        return rbe_build_id, rbe_build_json


def rbe_downloader(cfg: Path) -> remotetool.RemoteTool:
    with open(cfg) as f:
        downloader = remotetool.RemoteTool(remotetool.read_config_file_lines(f))
    return downloader


def download_artifact(
        downloader: remotetool.RemoteTool, digest: str,
        destination: Path) -> int:
    if destination.exists():
        destination.unlink()

    dl_result = downloader.download_blob(path=destination, digest=digest)
    if dl_result.returncode != 0:
        msg(f'Error downloading blob from CAS with digest {digest}')
        return dl_result.returncode

    return 0


def fetch_reproxy_log_from_bbid(
        bbpath: Path, bbid: str, verbose: bool = False) -> Path:
    bb = BuildBucketTool(bbpath)

    # Get the build that actually used RBE, and has an reproxy log.
    rbe_build_id, rbe_build_json = bb.get_rbe_build_info(bbid, verbose=verbose)
    if verbose:
        msg(f"Using build id {rbe_build_id} to look for reproxy logs")

    rpl_files = rbe_build_json['output']['properties'].get('rpl_files')

    if rpl_files is None:
        msg(f'Error looking up reproxy log from build {rbe_build_id}')
        return None

    # Assume there is only one.
    reproxy_log_name = rpl_files[0]
    if verbose:
        msg(f"reproxy log name: {reproxy_log_name}")

    # Fetch reproxy log (cached)
    reproxy_log_cache_path = bb.fetch_reproxy_log_cached(
        build_id=rbe_build_id,
        reproxy_log_name=reproxy_log_name,
        verbose=verbose,
    )
    # TODO: writing log out to disk and re-reading/parsing it can be slow.
    # Perhaps add an interface to take a string in-memory.
    return reproxy_log_cache_path


def fetch_artifact_from_reproxy_log(
    reproxy_log: Path,
    artifact_path: Path,
    cfg: Path,
    output: Path,
    verbose: bool = False,
) -> int:
    digest = reproxy_logs.lookup_output_file_digest(
        log=reproxy_log,
        path=artifact_path,
    )
    if digest is None:
        msg(f"Unable to find output file {artifact_path} in the reproxy log.")
        return 1

    if verbose:
        msg(f"Digest of {artifact_path} is {digest}")

    downloader = rbe_downloader(cfg)

    output = output or Path(artifact_path.name)
    exit_code = download_artifact(downloader, digest, output)
    if exit_code != 0:
        return exit_code

    msg(f"Artifact {artifact_path} downloaded to {output}.\n  digest: {digest}")
    return 0


def _main(
        bbpath: Path,
        cfg: Path,
        bbid: str = None,
        reproxy_log: Path = None,
        artifact_path: Path = None,
        output: Path = None,
        verbose: bool = False) -> int:
    reproxy_log = reproxy_log or fetch_reproxy_log_from_bbid(
        bbpath=bbpath,
        bbid=bbid,
        verbose=verbose,
    )
    if reproxy_log is None:
        return 1

    if artifact_path is None:
        # print the path to the log and exit without downloading anything
        msg(f'reproxy log: {reproxy_log}')
        return 0

    return fetch_artifact_from_reproxy_log(
        reproxy_log=reproxy_log,
        artifact_path=artifact_path,
        cfg=cfg,
        output=output or artifact_path.name,
        verbose=verbose,
    )


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)
    if args.output is not None and args.path is None:
        _MAIN_ARG_PARSER.error('-o requires --path')
        return 1
    try:
        return _main(
            bbpath=args.bb,
            bbid=args.bbid.lstrip('b') if args.bbid else None,
            artifact_path=args.path,
            reproxy_log=args.reproxy_log,
            cfg=args.cfg,
            output=args.output,
            verbose=args.verbose,
        )
    except BBError as e:
        msg(f"Error: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
