#!/usr/bin/env python3

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Run the Fuchsia Bazel SDK test suite in-tree.
You must have built the 'generate_fuchsia_sdk_repository'
target with 'fx build' before calling this script, as in:

  fx build generate_fuchsia_sdk_repository

You can use -- to separate the arguments that will be sent
to the underlying `bazel test` command. Useful for debugging
and experimentation.
"""

import argparse
import json
import os
import platform
import shlex
import sys
import subprocess
from pathlib import Path
from typing import Dict, Optional, Sequence, Tuple, Union

_HAS_FX = None

_VERBOSE = False

# Type alias for string or Path type.
# NOTE: With python 3.10+, it is possible to use 'str | Path' directly.
StrOrPath = Union[str, Path]


def _generate_command_string(
    args: Sequence[StrOrPath],
    env: Optional[Dict[str, str]] = None,
    cwd: Optional[Path] = None,
):
    """Generate a string that prints a command to be run.

    Args:
      args: a list of string or Path items corresponding to the command.
      env: optional dictionary for extra environment variables.
      cwd: optional Path for command's working directory.

    Returns:
      A string that can be printed to a terminal showing the command to
      run.
    """
    output = ""
    margin = ""
    wrap_command = False
    if cwd:
        margin = "  "
        output = f"(\n{margin}cd {cwd} &&\n"
        wrap_command = True

    if env:
        for key, value in sorted(env.items()):
            output += "%s%s=%s \\\n" % (margin, key, shlex.quote(value))

    for a in args:
        output += "%s%s\n" % (margin, shlex.quote(str(a)))

    if wrap_command:
        output += ")\n"

    return output


def _run_command(
    cmd_args: Sequence[StrOrPath],
    env: Optional[Dict[str, str]] = None,
    cwd: Optional[Path] = None,
) -> subprocess.CompletedProcess:
    """Run a given command.

    Args:
      args: a list of string or Path items corresponding to the command.
      env: optional dictionary for extra environment variables.
      cwd: optional Path for command's working directory.

    Returns:
      a subprocess.CompletedProcess value.
    """
    args = [str(a) for a in cmd_args]
    if _VERBOSE:
        print(
            "RUN_COMMAND:%s" %
            _generate_command_string(cmd_args, env=env, cwd=cwd))

    if env:
        new_env = os.environ.copy()
        new_env.update(env)
        env = new_env

    return subprocess.run(args, cwd=cwd, env=env)


def _print_error(msg: str) -> int:
    """Print error message to stderr then return 1."""
    print("ERROR: " + msg, file=sys.stderr)
    return 1


def _find_fuchsia_source_dir_from(path: Path) -> Optional[Path]:
    """Try to find the Fuchsia source directory from a starting location.

    Args:
      path: Path to a file or directory in the Fuchsia source tree.

    Returns:
      Path value for the Fuchsia source directory, or None if not found.
    """
    if path.is_file():
        path = path.parent

    path = path.resolve()
    while True:
        if str(path) == "/":
            return None
        if (path / ".jiri_manifest").exists():
            return path
        path = path.parent


def _find_fuchsia_build_dir(fuchsia_source_dir: Path) -> Optional[Path]:
    """Find the current Fuchsia build directory.

    Args:
      fuchsia_source_dir: Path value for the Fuchsia source directory.

    Returns:
      Path value for the current build directory selected with `fx` or None.
    """
    fx_build_dir = fuchsia_source_dir / ".fx-build-dir"
    if not fx_build_dir.exists():
        return None

    with open(fx_build_dir) as f:
        return fuchsia_source_dir / f.read().strip()


def _relative_path(path: Path):
    return Path(os.path.relpath(path))


def _depfile_quote(path: str) -> str:
    """Quote a path properly for depfiles, if necessary.

    shlex.quote() does not work because paths with spaces
    are simply encased in single-quotes, while the Ninja
    depfile parser only supports escaping single chars
    (e.g. ' ' -> '\ ').

    Args:
       path: input file path.
    Retursn:
       The input file path with proper quoting to be included
       directly in a depfile.
    """
    return path.replace("\\", "\\\\").replace(" ", "\\ ")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bazel", help="Specify bazel binary.")
    parser.add_argument(
        "--fuchsia_build_dir",
        help="Specify Fuchsia build directory (default is auto-detected).",
    )
    parser.add_argument(
        "--fuchsia_source_dir",
        help="Specify Fuchsia source directory (default is auto-detected).",
    )
    parser.add_argument(
        "--output_base", help="Use specific Bazel output base directory.")
    parser.add_argument(
        "--output_user_root",
        help="Use specific Bazel output user root directory.")
    parser.add_argument(
        "--stamp-file", help="Output stamp file, written on success only.")
    parser.add_argument("--depfile", help="Output Ninja depfile file.")
    parser.add_argument(
        "--target_cpu",
        help=
        "Target cpu name, using Fuchsia conventions (default is auto-detected).",
    )
    parser.add_argument(
        "--verbose", action="store_true", help="Enable verbose mode.")
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Do not print anything unless there is an error.")
    parser.add_argument(
        "--clean", action="store_true", help="Force clean build.")
    parser.add_argument(
        "--test_target",
        default="//:tests",
        help="Which target to invoke with `bazel test` (default is '//:tests')",
    )
    parser.add_argument("extra_args", nargs=argparse.REMAINDER)

    args = parser.parse_args()

    if args.quiet:
        args.verbose = None

    if args.verbose:
        global _VERBOSE
        _VERBOSE = True

    if args.depfile and not args.stamp_file:
        parser.error(
            "The --depfile option requires a --stamp-file output path!")

    extra_args = []
    if args.extra_args:
        if args.extra_args[0] != "--":
            parser.error(
                'Use "--" to separate  extra arguments passed to the bazel test command.'
            )
        extra_args = args.extra_args[1:]

    # Get Fuchsia source directory.
    if args.fuchsia_source_dir:
        fuchsia_source_dir = Path(args.fuchsia_source_dir)
    else:
        fuchsia_source_dir = _find_fuchsia_source_dir_from(Path(__file__))
        if not fuchsia_source_dir:
            return _print_error(
                "Cannot find Fuchsia source directory, please use --fuchsia_source_dir=DIR"
            )

    if not fuchsia_source_dir.exists():
        return _print_error(
            f"Fuchsia source directory does not exist: {fuchsia_source_dir}")

    # Get Fuchsia build directory.
    if args.fuchsia_build_dir:
        fuchsia_build_dir = Path(args.fuchsia_build_dir)
    else:
        fuchsia_build_dir = _find_fuchsia_build_dir(fuchsia_source_dir)
        if not fuchsia_build_dir:
            return _print_error(
                "Cannot auto-detect Fuchsia build directory, use --fuchsia_build_dir=DIR"
            )

    if not fuchsia_build_dir.exists():
        return _print_error(
            f"Fuchsia build directory does not exist: {fuchsia_build_dir}")

    fuchsia_build_dir = fuchsia_build_dir.resolve()

    # fuchsia_source_dir must be an absollute path or Bazel will complain
    # when it is used for --override_repository options below.
    fuchsia_source_dir = fuchsia_source_dir.resolve()

    # Compute Fuchsia host tag
    u = platform.uname()
    host_os = {
        "Linux": "linux",
        "Darwin": "mac",
        "Windows": "win",
    }.get(u.system, u.system)

    host_cpu = {
        "x86_64": "x64",
        "AMD64": "x64",
        "aarch64": "arm64",
    }.get(u.machine, u.machine)

    host_tag = f"{host_os}-{host_cpu}"

    # Find Bazel binary
    if args.bazel:
        bazel = Path(args.bazel)
    else:
        bazel = (
            fuchsia_source_dir / "prebuilt" / "third_party" / "bazel" /
            host_tag / "bazel")

    if not bazel.exists():
        return _print_error(f"Bazel binary does not exist: {bazel}")

    bazel = bazel.resolve()

    # Location of the prebuilt python toolchain.
    python_prebuilt_dir = (
        fuchsia_source_dir / "prebuilt" / "third_party" / "python3" / host_tag)

    # The Bazel workspace assumes that the Fuchsia cpu is the host
    # CPU unless --cpu or --platforms is used. Extract the target_cpu
    # from ${fuchsia_build_dir}/args.json and construct the corresponding
    # bazel test argument.
    #
    # Note that there is a subtle issue here: for historical reasons, the default
    # --cpu value is `k8`, an old moniker for the x86_64 cpu architecture. This
    # impacts the location of build artifacts in the default/target build
    # configuration, which would go under bazel-out/k8-fastbuild/bin/...
    # then.
    #
    # However, our --config=fuchsia_x64 argument below changes --cpu to `x86_64`
    # which is also recognized properly, but changes the location of build
    # artifacts to bazel-out/x86_64-fastbuild/ instead, which is important when
    # these paths go into build artifacts (e.g. product bundle manifests) and
    # need to be compared to golden files by the test suite.
    #
    # In short, this test suite does not support invoking `bazel test` without
    # an appropriate `--config=fuchsia_<cpu>` argument.
    #
    if args.target_cpu:
        target_cpu = args.target_cpu
    else:
        args_json = fuchsia_build_dir / "args.json"
        if not args_json.exists():
            return _print_error(
                "Cannot auto-detect target cpu, please use --target_cpu=CPU")

        with open(args_json) as f:
            target_cpu = json.load(f)["target_cpu"]

    # Assume this script is under '$WORKSPACE/scripts'
    script_dir = Path(__file__).parent.resolve()
    workspace_dir = script_dir.parent
    downloader_config_file = script_dir / "downloader_config"

    # These options must appear before the Bazel command
    bazel_startup_args = [
        bazel,
        # Disable parsing of $HOME/.bazelrc to avoid unwanted side-effects.
        "--nohome_rc",
    ]
    if args.output_user_root:
        output_user_root = Path(args.output_user_root).resolve()
        output_user_root.mkdir(parents=True, exist_ok=True)
        bazel_startup_args += [f"--output_user_root={output_user_root}"]

    if args.output_base:
        output_base = Path(args.output_base).resolve()
        output_base.mkdir(parents=True, exist_ok=True)
        bazel_startup_args += [f"--output_base={output_base}"]
    else:
        # Get output base from Bazel directly.
        output_base = Path(
            subprocess.check_output(
                [bazel, 'info', 'output_base'], text=True,
                cwd=workspace_dir).strip())

    # A map of repository override paths, required to prevent any downloads
    # during build and query operations.
    repo_override_map = {
        "bazel_skylib":
            fuchsia_source_dir / "third_party" / "bazel_skylib",
        "rules_cc":
            fuchsia_source_dir / "third_party" / "bazel_rules_cc",
        "rules_python":
            fuchsia_source_dir / "third_party" / "bazel_rules_python",
        "rules_license":
            fuchsia_source_dir / "third_party" / "bazel_rules_license",
        "platforms":
            fuchsia_source_dir / "third_party" / "bazel_platforms",
        "rules_java":
            fuchsia_source_dir / "build" / "bazel" / "local_repositories" /
            "rules_java",
        "remote_coverage_tools":
            fuchsia_source_dir / "build" / "bazel" / "local_repositories" /
            "remote_coverage_tools",
    }

    # These options must appear after the Bazel command.
    bazel_common_args = [
        # Prevent all downloads through a downloader configuration file.
        # Note that --experimental_repository_disable_download does not
        # seem to work at all.
        #
        # Fun fact: the path must be relative to the workspace, or an absolute
        # path, and if the file does not exist, the Bazel server will crash
        # *silently* with a Java exception, leaving no traces on the client
        # terminal :-(
        f"--experimental_downloader_config={downloader_config_file}",
    ]

    # Override repositories since all downloads are forbidden.
    # This allows the original WORKSPACE.bazel to work out-of-tree by
    # download repositories as usual, while running the test suite in-tree
    # will use the Fuchsia checkout's versions of these external dependencies.
    bazel_common_args += [
        f"--override_repository={name}={path}"
        for name, path in repo_override_map.items()
    ]

    # These argument remove verbose output from Bazel, used in queries.
    bazel_quiet_args = [
        "--noshow_loading_progress",
        "--noshow_progress",
        "--ui_event_filters=-info",
    ]

    # These options must appear for commands that act on the configure graph (i.e. all except `bazel query`
    bazel_config_args = bazel_common_args + [
        # Ensure binaries are generated for the right Fuchsia CPU architecture.
        # Without this, @fuchsia_sdk rules assume that target_cpu == host_cpu,
        # and will use an incorrect output path prefix (i.e.
        # bazel-out/k8-fastbuild/ instead of bazel-out/x86_64-fastbuild/ on
        # x64 hosts, leading to golden file comparison failures later.
        f"--config=fuchsia_{target_cpu}",
        # Ensure the embedded JDK that comes with Bazel is always used
        # This prevents Bazel from downloading extra host JDKs from the
        # network, even when a project never uses Java-related  rules
        # (for some still-very-mysterious reasons!)
        "--java_runtime_version=embedded_jdk",
        "--tool_java_runtime_version=embedded_jdk",
        # Ensure outputs are writable (umask 0755) instead of readonly (0555),
        # which prevent removing output directories with `rm -rf`.
        # See https://fxbug.dev/121003
        "--experimental_writable_outputs",
    ]

    bazel_test_args = []

    if args.quiet:
        bazel_common_args += bazel_quiet_args
        bazel_test_args += [
            "--show_result=0",
            "--test_output=errors",
            "--test_summary=none",
        ]

    if args.clean:
        # Perform clean build
        ret = _run_command(
            bazel_startup_args + ["clean", "--expunge"], cwd=workspace_dir)
        if ret.returncode != 0:
            return _print_error(
                "Could not clean bazel output base?\n%s\n" % ret.stderr)

    PATH = os.environ["PATH"]

    IGNORED_REPO = Path("IGNORED")

    bazel_env = {
        # Pass the location of the Fuchsia build directory to the
        # @fuchsia_sdk repository rule. Note that using --action_env will
        # not work because this option only affects Bazel actions, and
        # not repository rules.
        "LOCAL_FUCHSIA_PLATFORM_BUILD": str(fuchsia_build_dir),
        # An undocumented, but widely used, environment variable that tells Bazel to
        # not auto-detect the host C++ installation. This makes workspace setup faster
        # and ensures this can be used on containers where GCC or Clang are not
        # installed (Bazel would complain otherwise with an error).
        "BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN": "1",
        # Ensure our prebuilt Python3 executable is in the PATH to run repository
        # rules that invoke Python programs correctly in containers or jails that
        # do not expose the system-installed one.
        "PATH": f"{python_prebuilt_dir}/bin:{PATH}",
    }

    # Setting USER is required to run Bazel, so force it to run on infra bots.
    if 'USER' not in os.environ:
        bazel_env['USER'] = 'unused-bazel-build-user'

    # NOTE: Mapping labels to repository inputs is considerably simpler than
    # //build/bazel/scripts/bazel_action.py because there are way less edge
    # cases to consider and because ignoring @prebuilt_python and
    # @fuchsia_clang entirely is safe, since these are already populated
    # by the GN build_fuchsia_sdk_repository target which is an input
    # dependency for running this script.
    repo_map = repo_override_map.copy()
    repo_map.update(
        {
            "fuchsia_sdk_common":
                fuchsia_source_dir / "build" / "bazel_sdk" /
                "bazel_rules_fuchsia" / "common",
            "fuchsia_sdk":
                fuchsia_source_dir / "build" / "bazel_sdk" /
                "bazel_rules_fuchsia",
            "prebuilt_python":
                IGNORED_REPO,
            "fuchsia_clang":
                IGNORED_REPO,
            "bazel_tools":
                IGNORED_REPO,
            "local_config_cc":
                IGNORED_REPO,
        })

    def decompose_bazel_label(label: str) -> Tuple[str, str, str]:
        """Decompose a Bazel label into repo_name, package_path, target_name."""
        if build_file.startswith("//"):
            target_path = build_file[2:]
            repo_name = ""
        elif build_file.startswith("@"):
            pos = build_file.find("//", 1)
            assert pos > 0, f"build file path has invalid repository root: {build_file}"
            repo_name = build_file[1:pos]
            target_path = build_file[pos + 2:]

            repo_dir = repo_map.get(repo_name, None)
            assert repo_dir, f"Unknown repository name in build file path: {build_file}\n" + \
              f"Please modify {__file__} to handle it!"
            # A special value of IGNORED means build files from this repository should
            # be ignored.
            if str(repo_dir) == "IGNORED":
                return None
        else:
            assert False, f"Invalid build file path: {build_file}"

        package_dir, colon, target_name = target_path.partition(":")
        if colon == ":":
            if package_dir:
                target_path = f"{package_dir}/{target_name}"
            else:
                target_path = target_name
        else:
            target_path = package_dir + "/" + os.path.basename(package_dir)

    def resolve_build_file(build_file: str) -> Optional[Path]:
        """Convert a build file path to a real Path or None if it should be ignored."""
        if build_file.startswith("//"):
            target_path = build_file[2:]
            repo_dir = workspace_dir
            repo_name = False
        elif build_file.startswith("@"):
            pos = build_file.find("//", 1)
            assert pos > 0, f"build file path has invalid repository root: {build_file}"
            repo_name = build_file[1:pos]
            target_path = build_file[pos + 2:]

            repo_dir = repo_map.get(repo_name, None)
            assert repo_dir, f"Unknown repository name in build file path: {build_file}\n" + \
              f"Please modify {__file__} to handle it!"
            # A special value of IGNORED means build files from this repository should
            # be ignored.
            if str(repo_dir) == "IGNORED":
                return None
        else:
            assert False, f"Invalid build file path: {build_file}"

        package_dir, colon, target_name = target_path.partition(":")
        if colon == ":":
            if package_dir:
                target_path = f"{package_dir}/{target_name}"
            else:
                target_path = target_name
        else:
            target_path = package_dir + "/" + os.path.basename(package_dir)

        final_path = repo_dir / target_path
        if final_path.exists():
            return final_path

        # Sometimes the path will point to a non-existent file, for example
        #
        #   @fuchsia_sdk//:api_version.bzl
        #     corresponds to a file generated by the repository rule that
        #     generates the @fuchsia_sdk repository.
        #
        #   //build/bazel_sdk/bazel_rules_fuchsia/api_version.bzl does
        #     not exist.
        #
        #   $OUTPUT_BASE/external/fuchsia/api_version.bzl is the actual
        #     location of that file.
        #
        if repo_name:
            external_repo_dir = output_base / 'external' / repo_name
            final_path = external_repo_dir / target_path
            if final_path.exists():
                return final_path.resolve()

        # This should not happen, but print an error message pointing to this
        # script in case it really does!
        assert False, f"Unknown input label, please update {__file__} to handle it: {build_file}"

    query_target = f"set({args.test_target})"

    def find_build_files():
        # Perform a query to retrieve all build files.
        query_env = os.environ.copy()
        query_env.update(bazel_env)
        query_args = (
            bazel_startup_args + ["query"] + bazel_common_args +
            bazel_quiet_args + ["buildfiles(deps(%s))" % query_target])
        ret = subprocess.run(
            query_args,
            capture_output=True,
            text=True,
            cwd=workspace_dir,
            env=query_env)
        ret.check_returncode()
        build_files = ret.stdout.splitlines()
        result = set()
        for b in build_files:
            resolved = resolve_build_file(b)
            if resolved:
                result.add(resolved)

        return result

    def find_source_files():
        # Perform a cquery to find all input source files.
        cquery_env = os.environ.copy()
        cquery_env.update(bazel_env)
        cquery_args = (
            bazel_startup_args + ["cquery"] + bazel_config_args +
            bazel_quiet_args + [
                "--output=label",
                'kind("source file", deps(%s))' % query_target,
            ])
        ret = subprocess.run(
            cquery_args,
            capture_output=True,
            text=True,
            cwd=workspace_dir,
            env=cquery_env,
        )
        if ret.returncode != 0:
            print("ERROR: " + ret.stderr, file=sys.stderr)
            ret.check_returncode()
        source_files = set()
        for l in ret.stdout.splitlines():
            path, space, label = l.partition(" ")
            assert space == " " and label == "(null)", f"Invalid source file line: {l}"
            resolved = resolve_build_file(path)
            if not resolved:
                continue
            # If the file is a symlink, find its real location
            resolved = resolved.resolve()
            source_files.add(resolved)

        return source_files

    ret = _run_command(
        bazel_startup_args + ["test"] + bazel_config_args + bazel_test_args +
        [args.test_target] + extra_args,
        env=bazel_env,
        cwd=workspace_dir,
    )
    ret.check_returncode()

    if args.stamp_file:
        with open(args.stamp_file, 'w') as f:
            f.write('')

    if args.depfile:
        outputs = [args.stamp_file]
        implicit_inputs = find_build_files() | find_source_files()
        implicit_inputs = [_relative_path(p) for p in implicit_inputs]
        with open(args.depfile, 'w') as f:
            f.write(
                '%s: %s\n' % (
                    ' '.join(_depfile_quote(p) for p in outputs), ' '.join(
                        _depfile_quote(str(p)) for p in implicit_inputs)))

    return 0


if __name__ == "__main__":
    sys.exit(main())
