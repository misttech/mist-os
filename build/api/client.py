#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A tool providing information about the Fuchsia build graph(s).

See https://fxbug.dev/42084664 for context.
"""

# TECHNICAL NOTE: Reduce imports to a strict minimum to keep startup time
# of this script as low a possible. You can always perform an import lazily
# only when you need it (e.g. see how json and difflib are imported below).
import argparse
import os
import sys
from pathlib import Path
from typing import List, Sequence

_SCRIPT_FILE = Path(__file__)
_SCRIPT_DIR = _SCRIPT_FILE.parent
_FUCHSIA_DIR = (_SCRIPT_DIR / ".." / "..").resolve()


# NOTE: Do not use dataclasses because its import adds 20ms of startup time
# which is _massive_ here.
class BuildApiModule:
    """Simple dataclass-like type describing a given build API module."""

    def __init__(self, name: str, file_path: Path):
        self.name = name
        self.path = file_path

    def get_content(self):
        """Return content as sttring."""
        return self.path.read_text()

    def get_content_as_json(self) -> object:
        """Return content as a JSON object + lazy-loads the 'json' module."""
        import json

        return json.load(self.path.open())


class BuildApiModuleList(object):
    def __init__(self, build_dir: Path):
        self._modules: List[BuildApiModule] = []
        self.list_path = build_dir / "build_api_client_info"
        if not self.list_path.exists():
            return

        for line in self.list_path.read_text().splitlines():
            name, equal, file_path = line.partition("=")
            assert equal == "=", f"Invalid format for input file: {list_path}"
            self._modules.append(BuildApiModule(name, build_dir / file_path))

        self._modules.sort(key=lambda x: x.name)  # Sort by name.
        self._map = {m.name: m for m in self._modules}

    def empty(self) -> bool:
        """Return True if modules list is empty."""
        return len(self._modules) == 0

    def modules(self) -> Sequence[BuildApiModule]:
        """Return the sequence of BuildApiModule instances, sorted by name."""
        return self._modules

    def find(self, name: str) -> BuildApiModule:
        """Find a BuildApiModule by name, return None if not found."""
        return self._map.get(name)

    def names(self) -> Sequence[str]:
        """Return the sorted list of build API module names."""
        return [m.name for m in self._modules]


def printerr(msg, **kwargs) -> int:
    """Like print() but sends output to sys.stderr by default, then return 1."""
    if "file" in kwargs:
        print("ERROR: " + msg, **kwargs)
    else:
        print("ERROR: " + msg, file=sys.stderr, **kwargs)
    return 1


def get_build_dir(fuchsia_dir: Path) -> Path:
    """Get current Ninja build directory."""
    file = fuchsia_dir / ".fx-build-dir"
    if not file.exists():
        return Path()
    return fuchsia_dir / file.read_text().strip()


def cmd_list(args: argparse.Namespace) -> int:
    """Implement the `list` command."""
    for name in args.modules.names():
        print(name)
    return 0


def cmd_print(args: argparse.Namespace) -> int:
    """Implement the `print` command."""
    module = args.modules.find(args.api_name)
    if not module:
        return printerr(
            f"Unknown build API module name {args.api_name}, must be one of:\n\n %s\n"
            % "\n ".join(args.modules.names())
        )

    if not module.path.exists():
        return printerr(
            f"Missing input file, please use `fx set` or `fx gen` command: {module.path}"
        )

    print(module.get_content())
    return 0


def cmd_print_all(args: argparse.Namespace) -> int:
    """Implement the `print_all` command."""
    result = {}
    for module in args.modules.modules():
        if module.name != "api":
            result[module.name] = {
                "file": os.path.relpath(module.path, args.build_dir),
                "json": module.get_content_as_json(),
            }
    import json

    if args.pretty:
        print(
            json.dumps(result, sort_keys=True, indent=2, separators=(",", ": "))
        )
    else:
        print(json.dumps(result, sort_keys=True))
    return 0


def main(main_args: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fuchsia-dir",
        default=_FUCHSIA_DIR,
        type=Path,
        help="Specify Fuchsia source directory.",
    )
    parser.add_argument(
        "--build-dir",
        type=Path,
        help="Specify Ninja build directory.",
    )

    subparsers = parser.add_subparsers(required=True, help="sub-command help")
    print_parser = subparsers.add_parser(
        "print", help="Print build API module content"
    )
    print_parser.add_argument("api_name", help="Name of build API module")
    print_parser.set_defaults(func=cmd_print)

    print_all_parser = subparsers.add_parser(
        "print_all",
        help="Print single JSON containing the content of all build API modules",
    )
    print_all_parser.add_argument(
        "--pretty", action="store_true", help="Pretty print the JSON output."
    )
    print_all_parser.set_defaults(func=cmd_print_all)

    list_parser = subparsers.add_parser(
        "list", help="Print list of all build API modules"
    )
    list_parser.set_defaults(func=cmd_list)

    args = parser.parse_args(main_args)

    if not args.build_dir:
        args.build_dir = get_build_dir(args.fuchsia_dir)

    if not args.build_dir.exists():
        return printerr(
            "Could not locate build directory, please use `fx set` command or use --build-dir=DIR",
        )

    args.modules = BuildApiModuleList(args.build_dir)
    if args.modules.empty():
        return printerr(
            f"Missing input file, did you run `fx gen` or `fx set`?: {args.modules.list_path}"
        )

    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
