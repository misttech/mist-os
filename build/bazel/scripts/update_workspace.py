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
import errno
import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, Callable

import check_ninja_build_plan
import compute_content_hash
import workspace_utils


def force_symlink(target_path: str, dst_path: str) -> None:
    """Create a symlink at |dst_path| that points to |target_path|."""
    dst_dir = os.path.dirname(dst_path)
    os.makedirs(dst_dir, exist_ok=True)
    target_path = os.path.relpath(target_path, dst_dir)
    try:
        os.symlink(target_path, dst_path)
    except OSError as e:
        if e.errno == errno.EEXIST:
            os.remove(dst_path)
            os.symlink(target_path, dst_path)
        else:
            raise


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


def content_hash_all_files(paths: list[str]) -> str:
    fstate = compute_content_hash.FileState()
    for path in paths:
        fstate.hash_source_path(Path(path))
    return fstate.content_hash


class GeneratedFiles(object):
    """Models the content of a generated Bazel workspace."""

    def __init__(self, files: dict[str, Any] = {}) -> None:
        self._files = files

    def _check_new_path(self, path: str) -> None:
        assert path not in self._files, (
            "File entry already in generated list: " + path
        )

    def add_symlink(self, dst_path: str, target_path: str) -> None:
        self._check_new_path(dst_path)
        self._files[dst_path] = {
            "type": "symlink",
            "target": target_path,
        }

    def add_file(
        self, dst_path: str, content: str, executable: bool = False
    ) -> None:
        self._check_new_path(dst_path)
        entry: dict[str, Any] = {
            "type": "file",
            "content": content,
        }
        if executable:
            entry["executable"] = True
        self._files[dst_path] = entry

    def add_file_hash(self, dst_path: str) -> None:
        self._check_new_path(dst_path)
        self._files[dst_path] = {
            "type": "content_hash",
            "hash": content_hash_all_files([dst_path]),
        }

    def add_top_entries(
        self,
        fuchsia_dir: str,
        subdir: str,
        excluded_file: Callable[[str], bool],
    ) -> None:
        for name in os.listdir(fuchsia_dir):
            if not excluded_file(name):
                self.add_symlink(
                    os.path.join(subdir, name), os.path.join(fuchsia_dir, name)
                )

    def to_json(self) -> str:
        """Convert to JSON file."""
        return json.dumps(self._files, indent=2, sort_keys=True)

    def write(self, out_dir: str) -> None:
        """Write workspace content to directory."""
        for path, entry in self._files.items():
            type = entry["type"]
            if type == "symlink":
                target_path = entry["target"]
                link_path = os.path.join(out_dir, path)
                force_symlink(target_path, link_path)
            elif type == "file":
                file_path = os.path.join(out_dir, path)
                os.makedirs(os.path.dirname(file_path), exist_ok=True)
                with open(file_path, "w") as f:
                    f.write(entry["content"])
                if entry.get("executable", False):
                    os.chmod(file_path, 0o755)
            elif type == "content_hash":
                # Nothing to do here.
                pass
            else:
                assert False, "Unknown entry type: " % entry["type"]


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


def dot_bazelrc_format_args(
    workspace_dir: str,  # path
    logs_dir: str,  # path
    topdir: str,  # path
    default_platform: str,
    host_platform: str,
) -> dict[str, str]:
    """Returns a dictionary of .format args for expanding template.bazelrc.

    Args:
      workspace_dir: path to Bazel workspace
      logs_dir: path to Bazel logs dir
      topdir: GN output directory
      default_platform: one of the platforms named in build/bazel/platforms/BUILD.bazel
      host_platform: the host platform from build/bazel/platforms/BUILD.bazel
    """
    return {
        "default_platform": default_platform,
        "host_platform": host_platform,
        "workspace_log_file": os.path.relpath(
            os.path.join(logs_dir, "workspace-events.log"), workspace_dir
        ),
        "execution_log_file": os.path.relpath(
            os.path.join(logs_dir, "exec_log.pb.zstd"), workspace_dir
        ),
    }


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
    host_tag_alt = host_tag.replace("-", "_")

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

    generated = GeneratedFiles()

    def expand_template_file(filename: str, **kwargs: Any) -> str:
        """Expand a template file and add it to the set of tracked input files."""
        template_file = os.path.join(templates_dir, filename)
        generated.add_file_hash(os.path.abspath(template_file))
        with open(template_file) as f:
            return f.read().format(**kwargs)

    def write_workspace_file(path: str, content: str) -> None:
        generated.add_file(os.path.join("workspace", path), content)

    def create_workspace_symlink(path: str, target_path: str) -> None:
        generated.add_symlink(os.path.join("workspace", path), target_path)

    templates_dir = os.path.join(fuchsia_dir, "build", "bazel", "templates")

    if args.use_bzlmod:
        generated.add_file(
            os.path.join("workspace", "WORKSPACE.bazel"),
            "# Empty on purpose, see MODULE.bazel\n",
        )

        generated.add_symlink(
            os.path.join("workspace", "WORKSPACE.bzlmod"),
            os.path.join(
                fuchsia_dir, "build", "bazel", "toplevel.WORKSPACE.bzlmod"
            ),
        )

        generated.add_symlink(
            os.path.join("workspace", "MODULE.bazel"),
            os.path.join(
                fuchsia_dir, "build", "bazel", "toplevel.MODULE.bazel"
            ),
        )
    else:
        generated.add_symlink(
            os.path.join("workspace", "WORKSPACE.bazel"),
            os.path.join(
                fuchsia_dir, "build", "bazel", "toplevel.WORKSPACE.bazel"
            ),
        )

    # Generate symlinks

    generated.add_top_entries(
        fuchsia_dir, "workspace", workspace_utils.workspace_should_exclude_file
    )

    generated.add_symlink(
        os.path.join("workspace", "BUILD.bazel"),
        os.path.join(fuchsia_dir, "build", "bazel", "toplevel.BUILD.bazel"),
    )

    # The top-level .git directory must be symlinked because some actions actually
    # launch git commands (e.g. to generate a build version identifier). On the other
    # hand Jiri will complain if it finds a .git repository with Jiri metadata that
    # it doesn't know about in its manifest. The error looks like:
    #
    # ```
    # [17:49:48.200] WARN: Project "fuchsia" has path /work/fx-bazel-build, but was found in /work/fx-bazel-build/out/default/gen/build/bazel/output_base/execroot/main.
    # jiri will treat it as a stale project. To remove this warning please delete this or move it out of your root folder
    # ```
    #
    # Looking at the Jiri sources reveals that it is looking at a `.git/jiri` sub-directory
    # in all git directories it finds during a `jiri update` operation. To avoid the complaint
    # then symlink all $FUCHSIA_DIR/.git/ files and directories, except the 'jiri' one.
    # Also ignore the JIRI_HEAD / JIRI_LAST_BASE files to avoid confusion.
    fuchsia_git_dir = os.path.join(fuchsia_dir, ".git")
    for git_file in os.listdir(fuchsia_git_dir):
        if not (git_file == "jiri" or git_file.startswith("JIRI")):
            generated.add_symlink(
                "workspace/.git/" + git_file,
                os.path.join(fuchsia_git_dir, git_file),
            )

    # Generate a platform mapping file to ensure that using --platforms=<value>
    # also sets --cpu properly, as required by the Bazel SDK rules. See comments
    # in template file for more details.
    _BAZEL_CPU_MAP = {"x64": "k8", "arm64": "aarch64"}
    host_os = workspace_utils.get_host_platform()
    host_cpu = workspace_utils.get_host_arch()
    platform_mappings_content = expand_template_file(
        "template.platform_mappings",
        host_os=host_os,
        host_cpu=host_cpu,
        bazel_host_cpu=_BAZEL_CPU_MAP[host_cpu],
    )
    generated.add_file(
        os.path.join("workspace", "platform_mappings"),
        platform_mappings_content,
    )

    # Generate the content of .bazelrc
    bazelrc_content = expand_template_file(
        "template.bazelrc",
        **dot_bazelrc_format_args(
            workspace_dir=workspace_dir,
            logs_dir=logs_dir,
            topdir=topdir,
            default_platform=f"fuchsia_{args.target_arch}",
            host_platform=host_tag_alt,
        ),
    )
    if args.use_bzlmod:
        bazelrc_content += """
# Enable BlzMod, i.e. support for MODULE.bazel files.
common --experimental_enable_bzlmod
"""
    else:
        bazelrc_content += """
# Disable BzlMod, i.e. support for MODULE.bazel files, now that Bazel 7.0
# enables BzlMod by default.
common --enable_bzlmod=false
"""
    generated.add_file(os.path.join("workspace", ".bazelrc"), bazelrc_content)

    # Generate wrapper script in topdir/bazel that invokes Bazel with the right --output_base.
    bazel_launcher_content = expand_template_file(
        "template.bazel.sh",
        ninja_output_dir=os.path.relpath(gn_output_dir, topdir),
        ninja_prebuilt=os.path.relpath(ninja_binary, topdir),
        workspace=os.path.relpath(workspace_dir, topdir),
        bazel_bin_path=os.path.relpath(bazel_bin, topdir),
        logs_dir=os.path.relpath(logs_dir, topdir),
        python_prebuilt_dir=os.path.relpath(python_prebuilt_dir, topdir),
        output_base=os.path.relpath(output_base_dir, topdir),
        output_user_root=os.path.relpath(output_user_root, topdir),
    )
    generated.add_file("bazel", bazel_launcher_content, executable=True)

    # Ensure regeneration when this script's content changes!
    generated.add_file_hash(os.path.abspath(__file__))

    generated.add_symlink(
        os.path.join(
            "workspace",
            "fuchsia_build_generated",
            "assembly_developer_overrides.json",
        ),
        os.path.join(gn_output_dir, "gen", "assembly_developer_overrides.json"),
    )

    # LINT.IfChange
    generated.add_symlink(
        os.path.join("workspace", "fuchsia_build_generated", "args.json"),
        os.path.join(gn_output_dir, "args.json"),
    )
    # LINT.ThenChange(//build/bazel/repository_rules/fuchsia_build_info_repository.bzl)

    # LINT.IfChange
    # Create a symlink to the git host executable to make it accessible
    # when running a Bazel action on bots where it is not installed in
    # a standard location.
    git_bin_path = find_host_binary_path("git")
    assert git_bin_path, "Missing `git` program in current PATH!"
    generated.add_symlink(
        os.path.join("workspace", "fuchsia_build_generated", "git"),
        git_bin_path,
    )
    # LINT.ThenChange(//build/info/info.bzl)

    # LINT.IfChange
    # .jiri_root/ is not exposed to the workspace, but //build/info/BUILD.bazel
    # needs to access .jiri_root/update_history/latest so create a symlink just
    # for this file.
    generated.add_symlink(
        os.path.join(
            "workspace", "fuchsia_build_generated", "jiri_snapshot.xml"
        ),
        os.path.join(fuchsia_dir, ".jiri_root", "update_history", "latest"),
    )
    # LINT.ThenChange(//build/info/info.gni)

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
