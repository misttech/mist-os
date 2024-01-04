#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generate a remote_services.bazelrc file from a template and RBE config files.
"""

import argparse
import filecmp
import sys

from pathlib import Path
from typing import Dict, Iterable, Sequence


def read_rbe_config_file_lines(lines: Iterable[str]) -> Dict[str, str]:
    """Parser for reading RBE config files.

    RBE config files are text files with lines of "VAR=VALUE"
    (ignoring whole-line #-comments and blank lines).
    Spec details can be found at:
    https://github.com/bazelbuild/reclient/blob/main/internal/pkg/rbeflag/rbeflag.go

    Args:
      lines: lines of config from a file

    Returns:
      dictionary of key-value pairs read from the config file.
    """
    result = {}
    for line in lines:
        stripped = line.strip()
        if stripped and not stripped.startswith("#"):
            key, sep, value = stripped.partition("=")
            if sep == "=":
                result[key] = value
    return result


def read_rbe_config_file(cfg: Path) -> Dict[str, str]:
    return read_rbe_config_file_lines(cfg.read_text().splitlines())


def _rbe_config_to_template_substitutions(
    cfg: Dict[str, str]
) -> Dict[str, str]:
    instance = cfg["instance"]
    project = instance.split("/")[1]

    platform_values = cfg["platform"].split(",")
    platform_vars = {}
    for pv in platform_values:
        k, _, v = pv.partition("=")
        platform_vars[k] = v

    return {
        "remote_instance_name": instance,
        "rbe_project": project,
        "container_image": platform_vars.get("container-image", ""),
    }


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "--output",
        help="Output file to write expanded template",
        type=Path,
    )
    parser.add_argument(
        "--template",
        help="Text file containing Python template string",
        type=Path,
    )
    parser.add_argument(
        "--cfgs",
        help="Input configuration files to read",
        type=Path,
        nargs="*",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def _write_file_if_new_or_changed(path: Path, data: str):
    tmpf = Path(str(path) + ".tmp")  # assume this is writeable location
    tmpf.write_text(data)
    if not path.exists() or not filecmp.cmp(path, tmpf):
        tmpf.rename(path)
    else:  # contents are unchanged, leave old timestamp
        tmpf.unlink()


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)

    config = {}
    for f in args.cfgs:
        config.update(read_rbe_config_file(f))

    format_substitutions = _rbe_config_to_template_substitutions(config)

    template_text = args.template.read_text()

    formatted_text = template_text.format(**format_substitutions)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    _write_file_if_new_or_changed(args.output, formatted_text)

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
