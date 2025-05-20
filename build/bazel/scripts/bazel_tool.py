#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# IMPORTANT: This script should not depend on any Bazel workspace setup
# or specific environment. Only depend on standard Python3 modules
# and assume a minimum of Python3.8 is being used.

"""A tool to perform useful Bazel operations easily."""

import argparse
import re
import subprocess
import sys
from pathlib import Path


def error_message(msg: str) -> int:
    """Print error message to stderr, then return 1."""
    print(f"ERROR: {msg}", file=sys.stderr)
    return 1


def make_bazel_quiet_command(bazel: str, command: str) -> list[str]:
    """Create command argument list for a Bazel command that does not print too much.

    Args:
        bazel: Path to Bazel program.
        command: Bazel command (e.g. 'query', 'build', etc..)
    Returns:
        A sequence of strings that can be used as a command line prefix.
    """
    result = [
        bazel,
        command,
        "--noshow_loading_progress",
        "--noshow_progress",
        "--ui_event_filters=-info",
    ]
    if command != "query":
        result += ["--show_result=0"]
    return result


def cmd_target_dump(args: argparse.Namespace) -> int:
    """Implement the target_dump command."""
    bazel_cmd = (
        make_bazel_quiet_command(args.bazel, "query")
        + ["--output=build"]
        + args.target_set
    )
    buildifier_cmd = [
        args.buildifier,
        "--type=build",
        "--mode=fix",
        "--lint=off",
    ]
    proc1 = subprocess.Popen(
        bazel_cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=None
    )
    proc2 = subprocess.Popen(
        buildifier_cmd, stdin=proc1.stdout, stdout=None, stderr=None
    )
    proc1.wait()
    proc2.wait()
    if proc1.returncode != 0:
        return proc1.returncode
    return proc2.returncode


def cmd_actions(args: argparse.Namespace) -> int:
    """Implement the 'actions' command."""
    bazel_cmd = (
        make_bazel_quiet_command(args.bazel, "aquery")
        + args.extra_args[1:]
        + args.target_set
    )
    print("CMD %s" % bazel_cmd)
    ret = subprocess.run(
        bazel_cmd, stdin=subprocess.DEVNULL, capture_output=True, text=True
    )
    if ret.returncode != 0:
        print("%s\n" % ret.stdout, file=sys.stdout)
        print("%s\n" % ret.stderr, file=sys.stderr)
        return ret.returncode

    def ignored_path(path: str) -> bool:
        if path.startswith(
            (
                "external/prebuilt_clang/",
                "external/fuchsia_clang/",
                "prebuilt/third_party/sysroot/",
            )
        ):
            return True

        if path.find("external/fuchsia_prebuilt_rust/") >= 0:
            return True

        return False

    inputs_re = re.compile(r"^  Inputs: \[(.*)\]$")
    for line in ret.stdout.splitlines():
        m = inputs_re.match(line)
        if m:
            input_paths = []
            for path in m.group(1).split(","):
                path = path.strip()
                if not ignored_path(path):
                    input_paths.append(path)

            line = "  Inputs: [%s]" % (", ".join(input_paths))
        print(line)
    return 0


def cmd_set_gn_targets(args: argparse.Namespace) -> int:
    """Implement the set_gn_targets command."""

    # Lazy import of workspace_utils.py and gn_targets_utils.py
    sys.path.insert(0, str(Path(__file__).parent))
    import gn_targets_utils
    import workspace_utils

    verbosity = args.verbose - args.quiet

    def log(level: int, msg: str) -> None:
        if verbosity >= level:
            print(msg)

    # Determine Ninja build directory
    if args.build_dir:
        build_dir = Path(args.build_dir).resolve()
    else:
        build_dir = workspace_utils.find_fx_build_dir(args.fuchsia_dir)
        if not build_dir:
            return error_message(
                "Could not find Fuchsia build directory, use --build-dir=BUILD_DIR."
            )

        log(2, f"Found build directory: {build_dir}")

    bazel_target = args.bazel_target
    if not bazel_target.startswith(("//", "@")):
        return error_message(f"Invalid Bazel target label: {bazel_target}")
    if "(" in bazel_target:
        return error_message(
            f"Target label cannot use a GN toolchain suffix: {bazel_target}"
        )
    if bazel_target[0] == "@//":
        bazel_target = bazel_target[1:]

    def format_targets_list(targets: list[str]) -> str:
        """Helper to pretty-print a list of GN or Bazel targets."""
        return "\n".join(f"  {target}" for target in targets)

    # Load actions map then use it.
    actions_map = gn_targets_utils.BazelBuildActionsMap.FromBuildDir(build_dir)

    if args.bazel:
        bazel_args = [args.bazel]
    else:
        bazel_launcher = workspace_utils.find_bazel_launcher_path(
            args.fuchsia_dir, build_dir
        )
        if not bazel_launcher:
            return error_message("Cannot find Bazel launcher, use --bazel=PATH")
        bazel_args = [bazel_launcher]

    errors: list[str] = []

    gn_actions = gn_targets_utils.find_gn_bazel_action_infos_for(
        args.bazel_target,
        actions_map,
        bazel_args,
        log_step=lambda msg: log(2, msg),
        log_err=lambda msg: errors.append(msg),
    )

    if errors:
        print(f"ERROR: {errors[0]}", file=sys.stderr)
        for error in errors[1:]:
            print(error, file=sys.stderr)
        return 1

    if not gn_actions:
        return error_message(
            f"This Bazel target is not a dependency of any known GN bazel_action() target: {bazel_target}\n"
            + "\nIt should be a dependency of one of the following Bazel targets:\n"
            + format_targets_list(actions_map.bazel_targets)
            + "\n\nWhich are built by one of these GN targets:\n"
            + format_targets_list(actions_map.gn_targets)
            + "\n"
        )

    if len(gn_actions) > 1:
        top_level_bazel_targets = []
        gn_targets = []
        for gn_action in gn_actions:
            top_level_bazel_targets += gn_action.bazel_targets
            gn_targets.append(gn_action.gn_target)

        return error_message(
            f"Several GN targets depend on {bazel_target}\n"
            + "\nChoose a higher Bazel target, one of:\n"
            + format_targets_list(sorted(top_level_bazel_targets))
            + "\n\nWhich are built by one of these GN targets:\n"
            + format_targets_list(sorted(gn_targets))
            + "\n"
        )

    gn_action = gn_actions[0]
    if verbosity >= 1:  # Do not generate large strings if they are not needed.
        log(
            1,
            f"Bazel target {bazel_target} is a dependency of GN target {gn_action.gn_target}\nWhich builds the following bazel targets:\n%s\n"
            % format_targets_list(gn_action.bazel_targets),
        )

    gn_targets_dest = build_dir / gn_action.gn_targets_dir

    workspace_utils.force_symlink(
        Path(args.workspace) / workspace_utils.GN_TARGETS_DIR_SYMLINK,
        gn_targets_dest,
    )

    log(0, f"@gn_targets now points to {gn_targets_dest}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bazel", default="bazel", help="Specify bazel executable"
    )
    parser.add_argument(
        "--buildifier",
        default="buildifier",
        help="Specify buildifier executable",
    )
    parser.add_argument(
        "--workspace", type=Path, help="Specify workspace directory"
    )

    parser.add_argument(
        "--fuchsia-dir", type=Path, help="Specify path to Fuchsia directory"
    )

    parser.add_argument(
        "--build-dir", type=Path, help="Specify path to Ninja build directory"
    )

    subparsers = parser.add_subparsers(help="available commands")

    # The target_dump command.
    parser_target_dump = subparsers.add_parser(
        "target_dump",
        help="Dump definitions of Bazel targets.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=r"""
Print the real definition of a target, or set of targets, in a
Bazel graph, after all macros have been expanded. Note that:

- Each TARGET_SET argument is a standard Bazel target set expression
  (e.g. `//src/lib:foo` or `//:*`).

- The output is pretty printed for readability.

- For each target that is the result of a macro expansion, comments
  are added to describe the call chain that led to it.

- For each target generated by native.filegroup() in macros, new
  `generator_{function,location,name}` fields are added to indicate
  how it was generated.
""",
    )
    parser_target_dump.add_argument(
        "target_set",
        metavar="TARGET_SET",
        nargs="+",
        help="Set of targets to dump.",
    )
    parser_target_dump.set_defaults(func=cmd_target_dump)

    # The target_commands command.
    parser_actions = subparsers.add_parser(
        "actions",
        help="Dump action commands of Bazel targets.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=r"""
Print the action commands of a target, or set of targets, omitting
from the list of inputs sysroot and toolchain related headers, which
can be _very_ long when using the Fuchsia C++ toolchain.
""",
    )
    parser_actions.add_argument(
        "target_set",
        metavar="TARGET_SET",
        nargs="+",
        help="Set to targets to print actions for.",
    )
    parser_actions.add_argument(
        "--",
        dest="extra_args",
        default=[],
        nargs=argparse.REMAINDER,
        help="extra Bazel aquery-compatible arguments.",
    )
    parser_actions.set_defaults(func=cmd_actions)

    parser_set_gn_targets = subparsers.add_parser(
        "set_gn_targets",
        help="Update the @gn_targets symlink for a given Bazel target.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=r"""
Ensure the @gn_targets repository symlink points to the content required to
build a given Bazel target.

  fx bazel-tool set_gn_targets //vendor/acme/products/coyote:product_bundle

This is required before calling `fx bazel build <bazel_target>` directly
to ensure any filegroup in the @gn_targets repository points to the correct
up-to-date artifacts in the Ninja build directory.
""",
    )
    parser_set_gn_targets.add_argument(
        "-v",
        "--verbose",
        action="count",
        default=0,
        help="Increase verbosity for debugging.",
    )
    parser_set_gn_targets.add_argument(
        "--quiet", action="count", default=0, help="Decrease verbosity."
    )
    parser_set_gn_targets.add_argument(
        "bazel_target",
        metavar="BAZEL_TARGET",
        help="A Bazel target label, must begin with // or @",
    )
    parser_set_gn_targets.set_defaults(func=cmd_set_gn_targets)

    args = parser.parse_args()

    if not args.fuchsia_dir:
        # Assume this script is under //scripts/
        args.fuchsia_dir = Path(__file__).parent.parent.resolve()

    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
