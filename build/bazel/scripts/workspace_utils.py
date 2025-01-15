# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Misc utility functions related to the Bazel workspace."""

import errno
import json
import os
import sys
import typing as T
from pathlib import Path


def get_host_platform() -> str:
    """Return host platform name, following Fuchsia conventions."""
    if sys.platform == "linux":
        return "linux"
    elif sys.platform == "darwin":
        return "mac"
    else:
        return os.uname().sysname


def get_host_arch() -> str:
    """Return host CPU architecture, following Fuchsia conventions."""
    host_arch = os.uname().machine
    if host_arch == "x86_64":
        return "x64"
    elif host_arch.startswith(("armv8", "aarch64")):
        return "arm64"
    else:
        return host_arch


def get_host_tag() -> str:
    """Return host tag, following Fuchsia conventions."""
    return "%s-%s" % (get_host_platform(), get_host_arch())


def get_bazel_relative_topdir(
    fuchsia_dir: str | Path, workspace_name: str
) -> str:
    """Return Bazel topdir for a given workspace, relative to Ninja output dir."""
    input_file = os.path.join(
        fuchsia_dir,
        "build",
        "bazel",
        "config",
        f"{workspace_name}_workspace_top_dir",
    )
    assert os.path.exists(input_file), "Missing input file: " + input_file
    with open(input_file) as f:
        return f.read().strip()


def workspace_should_exclude_file(path: str) -> bool:
    """Return true if a file path must be excluded from the symlink list.

    Args:
        path: File path, relative to the top-level Fuchsia directory.
    Returns:
        True if this file path should not be symlinked into the Bazel workspace.
    """
    # Never symlink to the 'out' directory.
    if path == "out":
        return True
    # Don't symlink the Jiri files, this can confuse Jiri during an 'jiri update'
    # Don't symlink the .fx directory (TODO(digit): I don't remember why?)
    # Don´t symlink the .git directory as well, since it needs to be handled separately.
    if path.startswith((".jiri", ".fx", ".git")):
        return True
    # Don't symlink the convenience symlinks from the Fuchsia source tree
    if path in ("bazel-bin", "bazel-out", "bazel-repos", "bazel-workspace"):
        return True
    return False


def force_symlink(dst_path: str | Path, target_path: str | Path) -> None:
    """Create a symlink at |dst_path| that points to |target_path|.

    The generated symlink target will always be a relative path.

    Args:
        dst_path: path to symlink file to write or update.
        target_path: path to actual symlink target.
    """
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


# Type describing a callable that takes a file path as input and
# returns a content hash string for it as output.
FileHasherType: T.TypeAlias = T.Callable[[str | Path], str]


class GeneratedWorkspaceFiles(object):
    """Models the content of a generated Bazel workspace.

    Usage is:
      1. Create instance

      2. Optionally call set_file_hasher() if recording the content hash
         of input files is useful (see add_input_file()).

      3. Call the record_xxx() methods as many times as necessary to
         describe new workspace entries. This does not write anything
         to disk.

      4. Call to_json() to return a dictionary describing all added entries
         so far. This can be used to compare it to the result of a previous
         workspace generation.

      5. Call write() to populate the workspace with the recorded entries.
    """

    def __init__(self) -> None:
        self._files: T.Dict[str, T.Any] = {}
        self._file_hasher: T.Optional[FileHasherType] = None

    def set_file_hasher(self, file_hasher: FileHasherType) -> None:
        self._file_hasher = file_hasher

    def _check_new_path(self, path: str) -> None:
        assert path not in self._files, (
            "File entry already in generated list: " + path
        )

    def record_symlink(self, dst_path: str, target_path: str | Path) -> None:
        """Record a new symlink entry.

        Note that the entry always generates a relative symlink target
        when writing the entry to the workspace in write(), even if
        target_path is absolute.

        Args:
           dst_path: symlink path, relative to workspace root.
           target_path: symlink target path.
        """
        self._check_new_path(dst_path)
        self._files[dst_path] = {
            "type": "symlink",
            "target": str(target_path),
        }

    def record_file_content(
        self, dst_path: str, content: str, executable: bool = False
    ) -> None:
        """Record a new data file entry.

        Args:
            dst_path: file path, relative to workspace root.
            content: file content as a string.
            executable: optional flag, set to True to indicate this is an
               executable file (i.e. a script).
        """
        self._check_new_path(dst_path)
        entry: dict[str, T.Any] = {
            "type": "file",
            "content": content,
        }
        if executable:
            entry["executable"] = True
        self._files[dst_path] = entry

    def record_input_file_hash(self, input_path: str | Path) -> None:
        """Record the content hash of an input file.

        If set_file_hash_callback() was called, compute the content hash of
        a given input file path and record it. Note that nothing will be written
        to the workspace. This is only useful when comparing the output of
        to_json() between different generations to detect when the input file
        has changed.
        """
        input_file = str(input_path)
        self._check_new_path(input_file)
        if self._file_hasher:
            self._files[input_file] = {
                "type": "input_file",
                "hash": self._file_hasher(input_path),
            }

    def to_json(self) -> str:
        """Convert recorded entries to JSON string."""
        return json.dumps(self._files, indent=2, sort_keys=True)

    def write(self, out_dir: str | Path) -> None:
        """Write all recorded entries to a workspace directory."""
        for path, entry in self._files.items():
            type = entry["type"]
            if type == "symlink":
                target_path = entry["target"]
                link_path = os.path.join(out_dir, path)
                force_symlink(link_path, target_path)
            elif type == "file":
                file_path = os.path.join(out_dir, path)
                os.makedirs(os.path.dirname(file_path), exist_ok=True)
                with open(file_path, "w") as f:
                    f.write(entry["content"])
                if entry.get("executable", False):
                    os.chmod(file_path, 0o755)
            elif type == "input_file":
                # Nothing to do here.
                pass
            else:
                assert False, "Unknown entry type: " % entry["type"]


def record_fuchsia_workspace(
    generated: GeneratedWorkspaceFiles,
    top_dir: Path,
    fuchsia_dir: Path,
    gn_output_dir: Path,
    git_bin_path: Path,
    target_cpu: str,
    log: T.Optional[T.Callable[[str], None]] = None,
    enable_bzlmod: bool = False,
) -> None:
    """Record generated entries for the Fuchsia workspace and helper files.

    Note that this hards-code a few paths, i.e.:

      - The generated Bazel wrapper script is written to ${top_dir}/bazel
      - Bazel output base is at ${top_dir}/output_base
      - Bazel output user root is at ${top_dir}/output_user_root
      - The workspace goes into ${top_dir}/workspace/
      - Logs are written to ${top_dir}/logs/

    Args:
        generated: A GeneratedWorkspaceFiles instance modified by this function.
        top_dir: Path to the top-level
        fuchsia_dir: Path to the Fuchsia source checkout.
        gn_output_dir: Path to the GN/Ninja output directory.
        git_bin_path: Path to the host git binary to use during the build.
        target_cpu: The current build configuration's target cpu values,
            following Fuchsia conventions.
        log: Optional logging callback. If not None, must take a single
            string as argument.
        enable_bzlmod: Optional flag. Set to True to enable Bzlmod.
    """

    host_os = get_host_platform()
    host_cpu = get_host_arch()
    host_tag = get_host_tag()

    templates_dir = fuchsia_dir / "build" / "bazel" / "templates"

    logs_dir = top_dir / "logs"

    ninja_binary = (
        fuchsia_dir / "prebuilt" / "third_party" / "ninja" / host_tag / "ninja"
    )
    bazel_bin = (
        fuchsia_dir / "prebuilt" / "third_party" / "bazel" / host_tag / "bazel"
    )
    python_prebuilt_dir = (
        fuchsia_dir / "prebuilt" / "third_party" / "python3" / host_tag
    )
    output_base_dir = top_dir / "output_base"
    output_user_root = top_dir / "output_user_root"
    workspace_dir = top_dir / "workspace"
    bazel_launcher = (top_dir / "bazel").resolve()

    if log:
        log(
            f"""Using directories and files:
  Fuchsia:                {fuchsia_dir}
  GN build:               {gn_output_dir}
  Ninja binary:           {ninja_binary}
  Bazel source:           {bazel_bin}
  Top dir:                {top_dir}
  Logs directory:         {logs_dir}
  Bazel workspace:        {workspace_dir}
  Bazel output_base:      {output_base_dir}
  Bazel output user root: {output_user_root}
  Bazel launcher:         {bazel_launcher}
"""
        )

    def expand_template_file(filename: str, **kwargs: T.Any) -> str:
        """Expand a template file and add it to the set of tracked input files."""
        template_file = templates_dir / filename
        generated.record_input_file_hash(template_file.resolve())
        return template_file.read_text().format(**kwargs)

    def record_expanded_template(
        generated: GeneratedWorkspaceFiles,
        dst_path: str,
        template_name: str,
        **kwargs: T.Any,
    ) -> None:
        """Expand a template file and record its content.

        Args:
            generated: A GeneratedWorkspaceFiles instance.
            dst_path: Destination path for the recorded expanded content.
            template_name: Name of the template file to use.
            executable: Optional flag, set to True to indicate an executable file.
            **kwargs: Template expansion arguments.
        """
        executable = kwargs.get("executable", False)
        kwargs.pop("executable", None)
        content = expand_template_file(template_name, **kwargs)
        generated.record_file_content(dst_path, content, executable=executable)

    # Generate workspace/module files.
    if enable_bzlmod:
        generated.record_file_content(
            "workspace/WORKSPACE.bazel",
            "# Empty on purpose, see MODULE.bazel\n",
        )

        generated.record_symlink(
            "workspace/WORKSPACE.bzlmod",
            fuchsia_dir / "build" / "bazel" / "toplevel.WORKSPACE.bzlmod",
        )

        generated.record_symlink(
            "workspace/MODULE.bazel",
            fuchsia_dir / "build" / "bazel" / "toplevel.MODULE.bazel",
        )
    else:
        generated.record_symlink(
            "workspace/WORKSPACE.bazel",
            fuchsia_dir / "build" / "bazel" / "toplevel.WORKSPACE.bazel",
        )

    generated.record_symlink(
        "workspace/BUILD.bazel",
        fuchsia_dir / "build" / "bazel" / "toplevel.BUILD.bazel",
    )

    # Generate top-level symlinks
    for name in os.listdir(fuchsia_dir):
        if not workspace_should_exclude_file(name):
            generated.record_symlink(f"workspace/{name}", fuchsia_dir / name)

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
    fuchsia_git_dir = fuchsia_dir / ".git"
    for git_file in os.listdir(fuchsia_git_dir):
        if not (git_file == "jiri" or git_file.startswith("JIRI")):
            generated.record_symlink(
                "workspace/.git/" + git_file, fuchsia_git_dir / git_file
            )

    # Generate a platform mapping file to ensure that using --platforms=<value>
    # also sets --cpu properly, as required by the Bazel SDK rules. See comments
    # in template file for more details.
    _BAZEL_CPU_MAP = {"x64": "k8", "arm64": "aarch64"}
    host_os = get_host_platform()
    host_cpu = get_host_arch()
    host_tag = get_host_tag()

    record_expanded_template(
        generated,
        "workspace/platform_mappings",
        "template.platform_mappings",
        host_os=host_os,
        host_cpu=host_cpu,
        bazel_host_cpu=_BAZEL_CPU_MAP[host_cpu],
    )

    # Generate the content of .bazelrc
    logs_dir = top_dir / "logs"
    logs_dir_from_workspace = os.path.relpath(logs_dir, top_dir / "workspace")
    bazelrc_content = expand_template_file(
        "template.bazelrc",
        default_platform=f"fuchsia_{target_cpu}",
        host_platform=host_tag.replace("-", "_"),
        workspace_log_file=f"{logs_dir_from_workspace}/workspace_events.log",
        execution_log_file=f"{logs_dir_from_workspace}/exec_log.pb.zstd",
    )

    if not enable_bzlmod:
        bazelrc_content += """
# Disable BzlMod, i.e. support for MODULE.bazel files, now that Bazel 7.0
# enables BzlMod by default.
common --enable_bzlmod=false
"""
    generated.record_file_content("workspace/.bazelrc", bazelrc_content)

    # Generate wrapper script in topdir/bazel that invokes Bazel with the right --output_base.

    record_expanded_template(
        generated,
        "bazel",
        "template.bazel.sh",
        executable=True,
        ninja_output_dir=os.path.relpath(gn_output_dir, top_dir),
        ninja_prebuilt=os.path.relpath(ninja_binary, top_dir),
        workspace=os.path.relpath(workspace_dir, top_dir),
        bazel_bin_path=os.path.relpath(bazel_bin, top_dir),
        logs_dir=os.path.relpath(logs_dir, top_dir),
        python_prebuilt_dir=os.path.relpath(python_prebuilt_dir, top_dir),
        output_base=os.path.relpath(output_base_dir, top_dir),
        output_user_root=os.path.relpath(output_user_root, top_dir),
    )

    generated.record_symlink(
        os.path.join(
            "workspace",
            "fuchsia_build_generated",
            "assembly_developer_overrides.json",
        ),
        os.path.join(gn_output_dir, "gen", "assembly_developer_overrides.json"),
    )

    # LINT.IfChange
    generated.record_symlink(
        "workspace/fuchsia_build_generated/args.json",
        gn_output_dir / "args.json",
    )
    # LINT.ThenChange(//build/bazel/toplevel.WORKSPACE.bazel)

    # Create a symlink to the git host executable to make it accessible
    # when running a Bazel action on bots where it is not installed in
    # a standard location.
    generated.record_symlink(
        "workspace/fuchsia_build_generated/git",
        git_bin_path,
    )

    # LINT.IfChange
    # .jiri_root/ is not exposed to the workspace, but //build/info/BUILD.bazel
    # needs to access .jiri_root/update_history/latest so create a symlink just
    # for this file.
    generated.record_symlink(
        "workspace/fuchsia_build_generated/jiri_snapshot.xml",
        fuchsia_dir / ".jiri_root" / "update_history" / "latest",
    )
    # LINT.ThenChange(//build/info/info.gni)
