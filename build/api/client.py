#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A tool providing information about the Fuchsia build graph(s).

This is not intended to be called directly by developers, but by
specialized tools and scripts like `fx`, `ffx` and others.

See https://fxbug.dev/42084664 for context.
"""

# TECHNICAL NOTE: Reduce imports to a strict minimum to keep startup time
# of this script as low a possible. You can always perform an import lazily
# only when you need it (e.g. see how json and difflib are imported below).
import argparse
import os
import sys
import typing as T
from pathlib import Path

_SCRIPT_FILE = Path(__file__)
_SCRIPT_DIR = _SCRIPT_FILE.parent
_FUCHSIA_DIR = (_SCRIPT_DIR / ".." / "..").resolve()
sys.path.insert(0, str(_SCRIPT_DIR))


def _get_host_platform() -> str:
    """Return host platform name, following Fuchsia conventions."""
    if sys.platform == "linux":
        return "linux"
    elif sys.platform == "darwin":
        return "mac"
    else:
        return os.uname().sysname


def _get_host_arch() -> str:
    """Return host CPU architecture, following Fuchsia conventions."""
    host_arch = os.uname().machine
    if host_arch == "x86_64":
        return "x64"
    elif host_arch.startswith(("armv8", "aarch64")):
        return "arm64"
    else:
        return host_arch


def _get_host_tag() -> str:
    """Return host tag, following Fuchsia conventions."""
    return "%s-%s" % (_get_host_platform(), _get_host_arch())


def _warning(msg: str) -> None:
    """Print a warning message to stderr."""
    if sys.stderr.isatty():
        print(f"\033[1;33mWARNING:\033[0m {msg}", file=sys.stderr)
    else:
        print(f"WARNING: {msg}", file=sys.stderr)


def _error(msg: str) -> None:
    """Print an error message to stderr."""
    if sys.stderr.isatty():
        print(f"\033[1;31mERROR:\033[0m {msg}", file=sys.stderr)
    else:
        print(f"ERROR: {msg}", file=sys.stderr)


def _printerr(msg: str) -> int:
    """Like _error() but returns 1."""
    _error(msg)
    return 1


# NOTE: Do not use dataclasses because its import adds 20ms of startup time
# which is _massive_ here.
class BuildApiModule:
    """Simple dataclass-like type describing a given build API module."""

    def __init__(self, name: str, file_path: Path):
        self.name = name
        self.path = file_path

    def get_content(self) -> str:
        """Return content as sttring."""
        return self.path.read_text()

    def get_content_as_json(self) -> object:
        """Return content as a JSON object + lazy-loads the 'json' module."""
        import json

        return json.load(self.path.open())


class BuildApiModuleList(object):
    """Models the list of all build API module files."""

    def __init__(self, build_dir: Path):
        self._modules: T.List[BuildApiModule] = []
        self.list_path = build_dir / "build_api_client_info"
        if not self.list_path.exists():
            return

        for line in self.list_path.read_text().splitlines():
            name, equal, file_path = line.partition("=")
            assert (
                equal == "="
            ), f"Invalid format for input file: {self.list_path}"
            self._modules.append(BuildApiModule(name, build_dir / file_path))

        self._modules.sort(key=lambda x: x.name)  # Sort by name.
        self._map = {m.name: m for m in self._modules}

    def empty(self) -> bool:
        """Return True if modules list is empty."""
        return len(self._modules) == 0

    def modules(self) -> T.Sequence[BuildApiModule]:
        """Return the sequence of BuildApiModule instances, sorted by name."""
        return self._modules

    def find(self, name: str) -> BuildApiModule | None:
        """Find a BuildApiModule by name, return None if not found."""
        return self._map.get(name)

    def names(self) -> T.Sequence[str]:
        """Return the sorted list of build API module names."""
        return [m.name for m in self._modules]


class OutputsDatabase(object):
    """Manage a lazily-created / updated NinjaOutputsTabular database.

    Usage is:
        1) Create instance.
        2) Call load() to load the database from the Ninja build directory.
        3) Call gn_label_to_paths() or path_to_gn_label() as many times
           as needed.
    """

    def __init__(self) -> None:
        self._database: None | OutputsDatabase = None

    def load(self, build_dir: Path) -> bool:
        """Load the database from the given build directory.

        This takes care of converting the ninja_outputs.json file generated
        by GN into the more efficient tabular format, whenever this is needed.

        Args:
          build_dir: Ninja build directory.

        Returns:
          On success return True, on failure, print an error message to stderr
          then return False.
        """
        json_file = build_dir / "ninja_outputs.json"
        tab_file = build_dir / "ninja_outputs.tabular"
        if not json_file.exists():
            if tab_file.exists():
                tab_file.unlink()
            print(
                f"ERROR: Missing Ninja outputs file: {json_file}",
                file=sys.stderr,
            )
            return False

        from gn_ninja_outputs import NinjaOutputsTabular as OutputsDatabase

        database = OutputsDatabase()
        self._database = database

        if (
            not tab_file.exists()
            or tab_file.stat().st_mtime < json_file.stat().st_mtime
        ):
            # Re-generate database file when needed
            database.load_from_json(json_file)
            database.save_to_file(tab_file)
        else:
            # Load previously generated database.
            database.load_from_file(tab_file)
        return True

    def gn_label_to_paths(self, label: str) -> T.List[str]:
        assert self._database
        return self._database.gn_label_to_paths(label)

    def path_to_gn_label(self, path: str) -> str:
        assert self._database
        return self._database.path_to_gn_label(path)

    def target_name_to_gn_labels(self, target: str) -> T.List[str]:
        assert self._database
        return self._database.target_name_to_gn_labels(target)

    def is_valid_target_name(self, target: str) -> bool:
        assert self._database
        return self._database.is_valid_target_name(target)


def get_build_dir(fuchsia_dir: Path) -> Path:
    """Get current Ninja build directory."""
    # Use $FUCHSIA_DIR/.fx-build-dir if present. This is only useful
    # when invoking the script directly from the command-line, i.e.
    # during build system development.
    #
    # `fx` scripts should use the `fx-build-api-client` function which
    # always sets --build-dir to the appropriate value instead
    # (https://fxbug.dev/336720162).
    file = fuchsia_dir / ".fx-build-dir"
    if not file.exists():
        return Path()
    return fuchsia_dir / file.read_text().strip()


def get_ninja_path(fuchsia_dir: Path, host_tag: str) -> Path:
    return (
        fuchsia_dir / "prebuilt" / "third_party" / "ninja" / host_tag / "ninja"
    )


def cmd_list(args: argparse.Namespace) -> int:
    """Implement the `list` command."""
    for name in args.modules.names():
        print(name)
    return 0


class LastBuildApiFilter(object):
    """Filter one or more build API modules based on last build artifacts."""

    @staticmethod
    def add_parser_arguments(parser: argparse.ArgumentParser) -> None:
        """Add parser arguments related to filtering the output."""
        parser.add_argument(
            "--last-build-only",
            action="store_true",
            help="Only include values matching the targets from the last build invocation.",
        )
        parser.add_argument(
            "--no-last-build-check",
            action="store_true",
            help="When --last-build-only is used, ignore last build success check.",
        )

    @staticmethod
    def has_filter_flag(args: argparse.Namespace) -> bool:
        """Returns True if |args| contains a filtering flag."""
        return bool(args.last_build_only)

    def __init__(self, args: argparse.Namespace):
        self._error = ""

        # Verify that the last build was successful, and set error otherwise.
        if not args.no_last_build_check:
            if not (args.build_dir / "last_ninja_build_success.stamp").exists():
                self._error = "Last build did not complete successfully (use --ignore-last-build-check to ignore)."
                return

        self._ninja = get_ninja_path(args.fuchsia_dir, args.host_tag)
        self._filter = self.generate_filter(self._ninja, args.build_dir)

    @property
    def error(self) -> str:
        return self._error

    def filter_json(self, api_name: str, json_value: T.Any) -> T.Any:
        assert not self._error, "Cannot call filter_json() on error"
        return self._filter.filter_api_json(api_name, json_value)

    @staticmethod
    def generate_filter(ninja: Path, build_dir: Path) -> T.Any:
        import ninja_artifacts
        from build_api_filter import BuildApiFilter

        ninja_runner = ninja_artifacts.NinjaRunner(ninja)
        last_build_artifacts = ninja_artifacts.get_last_build_artifacts(
            build_dir, ninja_runner
        )
        last_build_sources = ninja_artifacts.get_last_build_sources(
            build_dir, ninja_runner
        )
        return BuildApiFilter(last_build_artifacts, last_build_sources)


def cmd_print(args: argparse.Namespace) -> int:
    """Implement the `print` command."""
    module = args.modules.find(args.api_name)
    if not module:
        return _printerr(
            f"Unknown build API module name {args.api_name}, must be one of:\n\n %s\n"
            % "\n ".join(args.modules.names())
        )

    if not module.path.exists():
        return _printerr(
            f"Missing input file, please use `fx set` or `fx gen` command: {module.path}"
        )

    content = module.get_content()
    if LastBuildApiFilter.has_filter_flag(args):
        import json

        api_filter = LastBuildApiFilter(args)
        if api_filter.error:
            print(f"ERROR: {api_filter.error}", file=sys.stderr)
            return 1

        json_content = api_filter.filter_json(
            args.api_name, json.loads(content)
        )
        content = json.dumps(json_content, indent=2, separators=(",", ": "))

    print(content)
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

    if LastBuildApiFilter.has_filter_flag(args):
        api_filter = LastBuildApiFilter(args)
        if api_filter.error:
            print(f"ERROR: {api_filter.error}", file=sys.stderr)
            return 1

        for name, v in result.items():
            v["json"] = api_filter.filter_json(name, v["json"])

    if args.pretty:
        print(
            json.dumps(result, sort_keys=True, indent=2, separators=(",", ": "))
        )
    else:
        print(json.dumps(result, sort_keys=True))
    return 0


class DebugSymbolCommandState(object):
    def __init__(
        self,
        modules: BuildApiModuleList,
        ninja: Path,
        build_dir: Path,
        resolve_build_ids: bool = True,
        test_mode: bool = False,
    ) -> None:
        import debug_symbols

        self._modules = modules
        self._ninja = ninja
        self._build_dir = build_dir
        self._debug_parser = debug_symbols.DebugSymbolsManifestParser(build_dir)

        if resolve_build_ids:
            self._debug_parser.enable_build_id_resolution()
        if test_mode:
            # --test-mode is used during regression testing to avoid
            # using a fake ELF input file. Simply return the file name
            # as the build-id value for now.
            def get_build_id(path: Path) -> str:
                return path.name

            self._debug_parser.set_build_id_callback_for_test(get_build_id)

    def parse_manifest(self, last_build_only: bool) -> int:
        module = self._modules.find("debug_symbols")
        assert module
        if not module.path.exists():
            return _printerr(
                f"Missing input file, please use `fx set` or `fx gen` command: {module.path}"
            )

        import json

        with module.path.open("rt") as f:
            manifest_json = json.load(f)

        if last_build_only:
            # Only filter the top-level entries, as Bazel-generated artifacts
            # have a "debug" path that points directly in the Bazel output_base
            # and are not known as Ninja artifacts.
            api_filter = LastBuildApiFilter.generate_filter(
                self._ninja, self._build_dir
            )
            manifest_json = api_filter.filter_api_json(
                "debug_symbols", manifest_json
            )

        try:
            self._debug_parser.parse_manifest_json(manifest_json, module.path)
        except ValueError as e:
            return _printerr(str(e))
        return 0

    @property
    def debug_symbol_entries(self) -> list[dict[str, T.Any]]:
        return self._debug_parser.entries


def cmd_print_debug_symbols(args: argparse.Namespace) -> int:
    state = DebugSymbolCommandState(
        args.modules,
        get_ninja_path(args.fuchsia_dir, args.host_tag),
        args.build_dir,
        args.resolve_build_ids,
        args.test_mode,
    )

    status = state.parse_manifest(bool(args.last_build_only))
    if status != 0:
        return status

    result = state.debug_symbol_entries

    import json

    if args.pretty:
        print(
            json.dumps(result, sort_keys=True, indent=2, separators=(",", ": "))
        )
    else:
        print(json.dumps(result, sort_keys=True))
    return 0


def cmd_export_last_build_debug_symbols(args: argparse.Namespace) -> int:
    import debug_symbols

    state = DebugSymbolCommandState(
        args.modules,
        get_ninja_path(args.fuchsia_dir, args.host_tag),
        args.build_dir,
        resolve_build_ids=True,  # Always resolve the .build-id value
        test_mode=args.test_mode,
    )

    state.parse_manifest(last_build_only=True)

    if args.no_breakpad_symbols_generation:
        dump_syms_tool = None
    else:
        dump_syms_tool = args.dump_syms
        if not dump_syms_tool:
            dump_syms_tool = (
                args.fuchsia_dir
                / f"prebuilt/third_party/breakpad/{args.host_tag}/dump_syms/dump_syms"
            )
            if not dump_syms_tool.exists():
                print(
                    f"ERROR: Missing breakpad tool, use --dump_syms=TOOL: {dump_syms_tool}"
                )

    def log_error(error: str) -> None:
        print(f"ERROR: {error}", file=sys.stderr)

    def log(msg: str) -> None:
        if not args.quiet:
            print(msg)

    exporter = debug_symbols.DebugSymbolExporter(
        args.build_dir,
        dump_syms_tool=dump_syms_tool,
        log=log,
        log_error=log_error,
    )
    exporter.parse_debug_symbols(state.debug_symbol_entries)

    if not exporter.export_debug_symbols(args.output_dir):
        return 1

    return 0


def cmd_last_ninja_artifacts(args: argparse.Namespace) -> int:
    """Implement the `print_last_ninja_artifacts` command."""
    import ninja_artifacts

    ninja = get_ninja_path(args.fuchsia_dir, args.host_tag)
    ninja_runner = ninja_artifacts.NinjaRunner(ninja)

    last_artifacts = ninja_artifacts.get_last_build_artifacts(
        args.build_dir, ninja_runner
    )

    print("\n".join(last_artifacts))
    return 0


def cmd_ninja_path_to_gn_label(args: argparse.Namespace) -> int:
    """Implement the `ninja_path_to_gn_label` command."""
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    failure = False
    labels = set()
    for path in args.paths:
        label = outputs.path_to_gn_label(path)
        if label:
            labels.add(label)
            continue

        if args.allow_unknown and not path.startswith("/"):
            labels.add(path)
            continue

        print(
            f"ERROR: Unknown Ninja target path: {path}",
            file=sys.stderr,
        )
        failure = True

    if failure:
        return 1

    print("\n".join(sorted(labels)))
    return 0


def cmd_ninja_target_to_gn_labels(args: argparse.Namespace) -> int:
    """Implement the `ninja_target_to_gn_labels` command."""
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    ninja_target = args.ninja_target
    if not outputs.is_valid_target_name(ninja_target):
        print(
            f"ERROR: Malformed Ninja target file name: {args.ninja_target}",
            file=sys.stderr,
        )
        return 1

    gn_labels = outputs.target_name_to_gn_labels(args.ninja_target)
    if gn_labels:
        print("\n".join(sorted(gn_labels)))
    return 0


def _get_target_cpu(build_dir: Path) -> str:
    args_json_path = build_dir / "args.json"
    if not args_json_path.exists():
        return "unknown_cpu"
    import json

    args_json = json.load(args_json_path.open())
    if not isinstance(args_json, dict):
        return "unknown_cpu"
    return args_json.get("target_cpu", "unknown_cpu")


def cmd_gn_label_to_ninja_paths(args: argparse.Namespace) -> int:
    """Implement the `gn_label_to_ninja_paths` command."""
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    from gn_labels import GnLabelQualifier

    host_cpu = args.host_tag.split("-")[1]
    target_cpu = _get_target_cpu(args.build_dir)
    qualifier = GnLabelQualifier(host_cpu, target_cpu)

    failure = False
    all_paths = []
    for label in args.labels:
        if label.startswith("//"):
            qualified_label = qualifier.qualify_label(label)
            paths = outputs.gn_label_to_paths(qualified_label)
            if paths:
                all_paths.extend(paths)
                continue
            _error(f"Unknown GN label (not in the configured graph): {label}")
            failure = True
        elif label.startswith("/"):
            _error(
                f"Absolute path is not a valid GN label or Ninja path: {label}"
            )
            failure = True
        elif args.allow_unknown:
            # Assume this is a Ninja path.
            all_paths.append(label)
        else:
            _error(f"Not a proper GN label (must start with //): {label}")
            failure = True

    if failure:
        return 1

    for path in sorted(all_paths):
        print(path)
    return 0


def cmd_fx_build_args_to_labels(args: argparse.Namespace) -> int:
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    from gn_labels import GnLabelQualifier

    host_cpu = args.host_tag.split("-")[1]
    target_cpu = _get_target_cpu(args.build_dir)
    qualifier = GnLabelQualifier(host_cpu, target_cpu)

    failure = False

    def ninja_path_to_gn_label(path: str) -> str:
        label = outputs.path_to_gn_label(path)
        if label:
            label_args = qualifier.label_to_build_args(label)
            _warning(
                f"Use '{' '.join(label_args)}' instead of Ninja path '{path}'"
            )
            return label

        error_msg = f"Unknown Ninja path: {path}"

        if args.allow_targets and outputs.is_valid_target_name(path):
            target_labels = outputs.target_name_to_gn_labels(path)
            if len(target_labels) == 1:
                label_args = qualifier.label_to_build_args(target_labels[0])
                _warning(
                    f"Use '{' '.join(label_args)}' instead of Ninja target '{path}'"
                )
                return target_labels[0]

            if len(target_labels) > 1:
                error_msg = (
                    f"Ambiguous Ninja target name '{path}' matches several GN labels:\n"
                    + "\n".join(target_labels)
                )
            else:
                error_msg = f"Unknown Ninja target: {path}"

        _error(error_msg)
        nonlocal failure
        failure = True
        return ""

    qualifier.set_ninja_path_to_gn_label(ninja_path_to_gn_label)

    labels = qualifier.build_args_to_labels(args.args)
    if failure:
        return 1

    for label in labels:
        print(label)

    return 0


def main(main_args: T.Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
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
    parser.add_argument(
        "--host-tag",
        help="Host platform tag, using Fuchsia conventions (auto-detected).",
        # NOTE: Do not set a default with _get_host_tag() here for faster startup,
        # since the //build/api/client wrapper script will always set this option.
    )
    subparsers = parser.add_subparsers(required=True, help="sub-command help.")
    print_parser = subparsers.add_parser(
        "print",
        help="Print build API module content.",
        description="Print the content of a given build API module, given its name. "
        "Use the 'list' command to print the list of all available names.",
    )
    LastBuildApiFilter.add_parser_arguments(print_parser)
    print_parser.add_argument("api_name", help="Name of build API module.")
    print_parser.set_defaults(func=cmd_print)

    print_all_parser = subparsers.add_parser(
        "print_all",
        help="Print single JSON containing the content of all build API modules.",
    )
    print_all_parser.add_argument(
        "--pretty", action="store_true", help="Pretty print the JSON output."
    )
    LastBuildApiFilter.add_parser_arguments(print_all_parser)
    print_all_parser.set_defaults(func=cmd_print_all)

    print_debug_symbols_parser = subparsers.add_parser(
        "print_debug_symbols",
        help="Print flattened debug symbol entries",
        description="Print the content of debug_symbols.json and all the files it includes "
        "as a single JSON list of entries.",
    )
    print_debug_symbols_parser.add_argument(
        "--pretty", action="store_true", help="Pretty print the JSON output."
    )
    print_debug_symbols_parser.add_argument(
        "--resolve-build-ids",
        action="store_true",
        help="Force resolution of build-id values.",
    )
    print_debug_symbols_parser.add_argument(
        "--test-mode", action="store_true", help="For regression tests only."
    )
    LastBuildApiFilter.add_parser_arguments(print_debug_symbols_parser)
    print_debug_symbols_parser.set_defaults(func=cmd_print_debug_symbols)

    last_ninja_artifacts_parser = subparsers.add_parser(
        "last_ninja_artifacts",
        help="Print the list of Ninja artifacts matching the last build invocation.",
    )
    last_ninja_artifacts_parser.set_defaults(func=cmd_last_ninja_artifacts)

    list_parser = subparsers.add_parser(
        "list",
        help="Print list of all build API module names.",
        description="Print list of all build API module names.",
    )
    list_parser.set_defaults(func=cmd_list)

    ninja_path_to_gn_label_parser = subparsers.add_parser(
        "ninja_path_to_gn_label",
        help="Print the GN label of a given Ninja output path.",
    )
    ninja_path_to_gn_label_parser.add_argument(
        "--allow-unknown",
        action="store_true",
        help="Keep unknown input Ninja paths in result.",
    )
    ninja_path_to_gn_label_parser.add_argument(
        "paths",
        metavar="NINJA_PATH",
        nargs="+",
        help="Ninja output path, relative to the build directory.",
    )
    ninja_path_to_gn_label_parser.set_defaults(func=cmd_ninja_path_to_gn_label)

    ninja_target_to_gn_labels_parser = subparsers.add_parser(
        "ninja_target_to_gn_labels",
        help="Find all GN labels that output a target with a given file name",
    )
    ninja_target_to_gn_labels_parser.add_argument(
        "ninja_target", help="Ninja target file name only."
    )
    ninja_target_to_gn_labels_parser.set_defaults(
        func=cmd_ninja_target_to_gn_labels
    )

    gn_label_to_ninja_paths_parser = subparsers.add_parser(
        "gn_label_to_ninja_paths",
        help="Print the Ninja output paths of one or more GN labels.",
        description="Print the Ninja output paths of one or more GN labels.",
    )
    gn_label_to_ninja_paths_parser.add_argument(
        "--allow-unknown",
        action="store_true",
        help="Keep unknown input GN labels in result.",
    )
    gn_label_to_ninja_paths_parser.add_argument(
        "labels",
        metavar="GN_LABEL",
        nargs="+",
        help="A qualified GN label (begins with //, may include full toolchain suffix).",
    )
    gn_label_to_ninja_paths_parser.set_defaults(
        func=cmd_gn_label_to_ninja_paths
    )

    fx_build_args_to_labels_parser = subparsers.add_parser(
        "fx_build_args_to_labels",
        help="Parse fx build arguments into qualified GN labels.",
        description="Convert a series of `fx build` arguments into a list of fully qualified GN labels.",
    )
    fx_build_args_to_labels_parser.add_argument(
        "--allow-targets",
        action="store_true",
        help="Try to convert Ninja target file names (not paths) to GN labels.",
    )
    fx_build_args_to_labels_parser.add_argument(
        "--args", required=True, nargs=argparse.REMAINDER
    )
    fx_build_args_to_labels_parser.set_defaults(
        func=cmd_fx_build_args_to_labels
    )

    export_last_build_debug_symbols_parser = subparsers.add_parser(
        "export_last_build_debug_symbols",
        help="Export the debug symbols from last build's artifacts",
        description="Export the ELF debug symbol files, and optional breakpad symbol ones, from last build's artifacts.",
    )
    export_last_build_debug_symbols_parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Output directory, this will be cleaned before generation.",
    )
    export_last_build_debug_symbols_parser.add_argument(
        "--no-breakpad-symbols-generation",
        action="store_true",
        help="Do not try to generate breakpad symbol files in the output. Ignores --dump_syms.",
    )
    export_last_build_debug_symbols_parser.add_argument(
        "--dump_syms",
        type=Path,
        help="Path to Breakpad dump_syms binary (auto-detected), used by --with-breakpad-symbols.",
    )
    export_last_build_debug_symbols_parser.add_argument(
        "--quiet",
        action="store_true",
        help="Do not print details of operations.",
    )
    export_last_build_debug_symbols_parser.add_argument(
        "--test-mode", action="store_true", help="For regression tests only."
    )
    export_last_build_debug_symbols_parser.set_defaults(
        func=cmd_export_last_build_debug_symbols
    )

    args = parser.parse_args(main_args)

    if not args.build_dir:
        args.build_dir = get_build_dir(args.fuchsia_dir)

    if not args.build_dir.exists():
        return _printerr(
            "Could not locate build directory, please use `fx set` command or use --build-dir=DIR.",
        )

    if not args.host_tag:
        args.host_tag = _get_host_tag()

    args.modules = BuildApiModuleList(args.build_dir)
    if args.modules.empty():
        return _printerr(
            f"Missing input file, did you run `fx gen` or `fx set`?: {args.modules.list_path}"
        )

    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
