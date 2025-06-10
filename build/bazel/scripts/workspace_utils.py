# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Misc utility functions related to the Bazel workspace."""

import errno
import json
import os
import stat
import sys
import typing as T
from pathlib import Path

# Location of the @gn_targets redirection symlink, relative
# to the Bazel workspace.
GN_TARGETS_DIR_SYMLINK = "fuchsia_build_generated/gn_targets_dir"

# Separation character used by Bazel for extension-generated repo names.
#
# See https://bazel.build/external/extension#repository_names_and_visibility
_BAZEL_REPO_NAME_SEPARATOR = "+"


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


def find_fuchsia_dir(from_path: T.Optional[Path] = None) -> Path:
    """Find the Fuchsia checkout from a specific path.

    Args:
        from_path: Optional starting path for search. Defaults to the current directory.
    Returns:
        Path to the Fuchsia checkout directory (absolute).
    Raises:
        ValueError if the path could not be found.
    """
    start_path = from_path.resolve() if from_path else Path(os.getcwd())
    cur_path = start_path
    while True:
        if (cur_path / ".jiri_manifest").exists():
            return cur_path
        prev_path = cur_path
        cur_path = cur_path.parent
        if cur_path == prev_path:
            raise ValueError(
                f"Could not find Fuchsia checkout directory from: {start_path}"
            )


def find_fx_build_dir(fuchsia_dir: Path) -> T.Optional[Path]:
    """Find the build directory set through 'fx set' or 'fx use'.

    Args:
       fuchsia_dir: Path to Fuchsia checkout directory.
    Returns:
       Path to build directory if found, of None if none
       is available (e.g. fresh checkout or infra build).
    """
    fx_build_dir_file = fuchsia_dir / ".fx-build-dir"
    if fx_build_dir_file.exists():
        build_dir_relative = fx_build_dir_file.read_text().strip()
        if build_dir_relative:
            build_dir = fuchsia_dir / build_dir_relative
            if build_dir.exists():
                return build_dir
    return None


def get_bazel_relative_topdir(fuchsia_dir: str | Path) -> tuple[str, set[Path]]:
    """Return Bazel topdir, relative to Ninja output dir.

    Args:
        fuchsia_dir: Fuchsia source directory path.
    Returns:
        A (topdir, input_files) pair, where input_files is a set of Path
        values corresponding to the file(s) read by this function.
    """
    input_file = os.path.join(
        fuchsia_dir,
        "build",
        "bazel",
        "config",
        "bazel_top_dir",
    )
    assert os.path.exists(input_file), "Missing input file: " + input_file
    with open(input_file) as f:
        return f.read().strip(), {Path(input_file)}


def find_bazel_launcher_path(
    fuchsia_dir: Path, build_dir: Path
) -> T.Optional[Path]:
    """Find the path of the Bazel launcher script.

    Args:
        fuchsia_dir: Path to Fuchsia checkout directory.
        build_dir: Path to Fuchsia build directory.

    Returns:
        Path to bazel launcher script, or empty Path() value if the file
        does not exist.
    """
    bazel_topdir, _ = get_bazel_relative_topdir(fuchsia_dir)
    result = build_dir / bazel_topdir / "bazel"
    return result if result.exists() else None


def find_bazel_workspace_path(
    fuchsia_dir: Path, build_dir: Path
) -> T.Optional[Path]:
    """Find the path of the Bazel workspace.

    Args:
        fuchsia_dir: Path to Fuchsia checkout directory.
        build_dir: Path to Fuchsia build directory.

    Returns:
        Path to bazel workspace, or None if the directory does not exists.
    """
    bazel_topdir, _ = get_bazel_relative_topdir(fuchsia_dir)
    result = build_dir / bazel_topdir / "workspace"
    return result if result.exists() else None


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
    target_path = os.path.relpath(target_path, dst_dir)
    return force_raw_symlink(dst_path, target_path)


def force_raw_symlink(dst_path: str | Path, target_path: str | Path) -> None:
    dst_dir = os.path.dirname(dst_path)
    os.makedirs(dst_dir, exist_ok=True)
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
        self._files: dict[str, T.Any] = {}
        self._file_hasher: T.Optional[FileHasherType] = None
        self._input_files: set[Path] = set()

    def set_file_hasher(self, file_hasher: FileHasherType) -> None:
        self._file_hasher = file_hasher

    def _check_new_path(self, path: str) -> None:
        assert path not in self._files, (
            "File entry already in generated list: " + path
        )

    @property
    def input_files(self) -> set[Path]:
        """The set of input file Paths that were read through read_text_file()."""
        return self._input_files

    def read_text_file(self, path: Path) -> str:
        """Read an input file and return its content as text.

        This also ensures that the file is tracked as an input file
        to later be returned through the self.input_files property.

        Args:
            path: Input file path.
        Returns:
            content of input path, as a string.
        """
        path = path.resolve()
        self._input_files.add(path)
        self.record_input_file_hash(path)
        return path.read_text()

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

    def record_raw_symlink(
        self, dst_path: str, target_path: str | Path
    ) -> None:
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
            "type": "raw_symlink",
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
            elif type == "raw_symlink":
                target_path = entry["target"]
                link_path = os.path.join(out_dir, path)
                force_raw_symlink(link_path, target_path)
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

    def update_if_needed(self, out_dir: Path, manifest_path: Path) -> bool:
        """Write all recorded entries if they differ from the content of a given manifest.

        If the manifest exists and its content matches the recorded entries,
        then this method does not do anything. Otherwise, it will overwrite
        the manifests with new values, clean the output directory, and re-populate
        it entirely with new content.

        Args:
            out_dir: Output directory to update if needed.
            manifest_path: Path to manifest file to use for comparisons.
        Returns:
            True if the output directory and manifest were updated, False otherwise.
        """
        current_manifest = self.to_json()
        if out_dir.is_dir() and manifest_path.exists():
            if manifest_path.read_text() == current_manifest:
                # Nothing to change here.
                return False

        manifest_path.write_text(current_manifest)
        create_clean_dir(str(out_dir))
        self.write(out_dir)
        return True


def record_fuchsia_workspace(
    generated: GeneratedWorkspaceFiles,
    top_dir: Path,
    fuchsia_dir: Path,
    gn_output_dir: Path,
    git_bin_path: Path,
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
            f"""- Using directories and files:
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
  Git binary path:        {git_bin_path}"""
        )

    def expand_template_file(
        generated: GeneratedWorkspaceFiles, filename: str, **kwargs: T.Any
    ) -> str:
        """Expand a template file and add it to the set of tracked input files."""
        template_file = templates_dir / filename
        return generated.read_text_file(template_file).format(**kwargs)

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
        content = expand_template_file(generated, template_name, **kwargs)
        generated.record_file_content(dst_path, content, executable=executable)

    # Generate workspace/module files.
    if enable_bzlmod:
        generated.record_symlink(
            "workspace/WORKSPACE.bzlmod",
            fuchsia_dir / "build" / "bazel" / "toplevel.WORKSPACE.bzlmod",
        )

        generated.record_symlink(
            "workspace/MODULE.bazel",
            fuchsia_dir / "build" / "bazel" / "toplevel.MODULE.bazel",
        )
    else:
        # Generate two distinct Bazel workspace definition files.
        #
        # - WORKSPACE.no_sdk.bazel + no_sdk.bazelrc:
        #
        #   Omits any repository definition related to the IDK and the SDK.
        #   I.e. there is no @fuchsia_in_tree_idk or @fuchsia_sdk repository
        #   in this workspace. This allows building Bazel targets immediately
        #   after `fx gen`, as long as they do not reference labels pointing
        #   to @fuchsia_sdk or @fuchsia_in_tree_idk. This is used to implement
        #   `fx bazel-no-sdk` and the `no_sdk = true` attribute in the GN
        #   bazel_action() template.
        #
        # - WORKSPACE.fuchsia.bazel + fuchsia.bazelrc:
        #
        #   Contains the full set of repository definitions, which currently
        #   require the IDK to be built with Ninja before being able to use the
        #   workspace, even for simpler queries. Used to implement `fx bazel`
        #   and the GN bazel_action() without `no_sdk = true`.
        #
        # The WORKSPACE.bazel and .bazelrc files are simple symlinks to either
        # one of these set of files, and will be adjusted before invoking Bazel
        # by `fx bazel`, `fx bazel-no-sdk` and `bazel_action.py`.
        #

        # This file contains all definitions, with a special marker line.
        # workspace/WORKSPACE.fuchsia.bazel is a symlink to it, then its
        # content is processed and cut to generate workspace/WORKSPACE.no_sdk.bazel.
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
        bazel_host_cpu=_BAZEL_CPU_MAP.get(host_cpu, host_cpu),
        host_cpu=host_cpu,
        host_os=host_os,
    )

    # Generate the content of .bazelrc
    logs_dir = top_dir / "logs"
    logs_dir_from_workspace = os.path.relpath(logs_dir, top_dir / "workspace")
    bazelrc_content = expand_template_file(
        generated,
        "template.bazelrc",
        workspace_log_file=f"{logs_dir_from_workspace}/workspace_events.log",
        execution_log_file=f"{logs_dir_from_workspace}/exec_log.pb.zstd",
    )

    if enable_bzlmod:
        bazelrc_content = bazelrc_content.replace(
            "--enable_bzlmod=false", "--enable_bzlmod=true"
        )

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
    # LINT.ThenChange(//build/bazel_sdk/bazel_rules_fuchsia/common/fuchsia_platform_build.bzl)

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

    generated.record_symlink(
        # LINT.IfChange
        "workspace/fuchsia_build_generated/fuchsia_in_tree_idk.hash",
        # LINT.ThenChange(//build/bazel/toplevel.WORKSPACE.bzlmod)
        # LINT.IfChange
        gn_output_dir / "sdk/prebuild/in_tree_collection.json",
        # LINT.ThenChange(//build/regenerator.py)
    )

    # Used when merging the IDK sub-build directories. For other use cases,
    # prefer a symlink with a narrower scope.
    generated.record_symlink(
        # LINT.IfChange
        "workspace/fuchsia_build_generated/ninja_root_build_dir",
        # LINT.ThenChange(//build/bazel/bazel_sdk/BUILD.bazel)
        gn_output_dir,
    )

    generated.record_symlink(
        # LINT.IfChange
        "workspace/fuchsia_build_generated/fuchsia_internal_only_idk.hash",
        # LINT.ThenChange(//build/bazel/toplevel.WORKSPACE.bzlmod)
        # LINT.IfChange
        gn_output_dir / "obj/build/bazel/fuchsia_internal_only_idk.hash",
        # LINT.ThenChange(//build/bazel/BUILD.gn)
    )

    # The following symlinks are used only by bazel_action.py when processing
    # the list of Bazel source inputs, the actual repository setup in
    # WORKSPACE.bazel reuses the two symlinks above instead.
    generated.record_symlink(
        # LINT.IfChange
        "workspace/fuchsia_build_generated/fuchsia_sdk.hash",
        # LINT.ThenChange(//build/bazel/scripts/bazel_action.py)
        # LINT.IfChange
        gn_output_dir / "sdk/prebuild/in_tree_collection.json",
        # LINT.ThenChange(//build/regenerator.py)
    )

    generated.record_symlink(
        # LINT.IfChange
        "workspace/fuchsia_build_generated/internal_sdk.hash",
        # LINT.ThenChange(//build/bazel/scripts/bazel_action.py)
        # LINT.IfChange
        gn_output_dir / "obj/build/bazel/fuchsia_internal_only_idk.hash",
        # LINT.ThenChange(//build/bazel/BUILD.gn)
    )

    # Create a link to an empty repository. This is updated by bazel_action.py
    # before each Bazel invocation to point to the @gn_targets content specific
    # to its parent bazel_action() target.
    generated.record_symlink(
        f"workspace/{GN_TARGETS_DIR_SYMLINK}",
        fuchsia_dir / "build" / "bazel" / "local_repositories" / "empty",
    )


def generate_fuchsia_workspace(
    fuchsia_dir: Path,
    build_dir: Path,
    log: T.Optional[T.Callable[[str], None]] = None,
    enable_bzlmod: bool = False,
) -> set[Path]:
    """Generate the Fuchsia Bazel workspace and associated files.

    Args:
        fuchsia_dir: Path to the Fuchsia source checkout.
        build_dir: Path to the GN/Ninja output directory.
        log: Optional logging callback. If not None, must take a single
            string as argument.
        enable_bzlmod: Optional flag. Set to True to enable Bzlmod.

    Returns:
       A set of input file paths that were read by this function.
    """
    # Find path of host git tool
    git_bin_path = find_host_binary_path("git")
    assert git_bin_path, f"Could not find 'git' in current PATH!"

    # # Extract target_cpu from args.json. This file is GN-generated
    # # and doesn't need to be added to input_files.
    # args_json_path = build_dir / "args.json"
    # assert args_json_path.exists(), f"Missing GN output file: {args_json_path}"
    # with args_json_path.open("rb") as f:
    #     args_json = json.load(f)
    #
    # target_cpu = args_json.get("target_cpu")
    # assert target_cpu, f"Missing target_cpu key in {args_json_path}"

    # Find the location of the Bazel top-dir relative to the Ninja
    # build directory.
    bazel_top_dir, input_files = get_bazel_relative_topdir(fuchsia_dir)

    # Generate the bazel launcher and Bazel workspace files.
    generated = GeneratedWorkspaceFiles()

    top_dir = build_dir / bazel_top_dir

    record_fuchsia_workspace(
        generated,
        top_dir=top_dir,
        fuchsia_dir=fuchsia_dir,
        gn_output_dir=build_dir,
        git_bin_path=Path(git_bin_path),
        log=log,
        enable_bzlmod=True,
    )

    # Remove the old workspace's content, which is just a set
    # of symlinks and some auto-generated files.
    create_clean_dir(str(top_dir / "workspace"))

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
    create_clean_dir(os.path.join(top_dir / "output_base" / "external"))

    generated.write(top_dir)

    return input_files | generated.input_files


class GnBuildArgs(object):
    """A class to handle `gn_build_variables_for_bazel.json` input files.

    These files are used to export the values of GN args to an `args.bzl` file
    and optionally `vendor_<name>_args.bzl` files in the @fuchsia_build_info
    Bazel repository.

    See the comments for //build/bazel:gn_build_variables_for_bazel for the
    format of the `gn_build_variables_for_bazel.json` files.

    Vendor-specific files should always be placed at
        $OUT_DIR/vendor_<name>_gn_build_variables_for_bazel.json
    and the target that generates them must always be in the dependency graph
    when building anything from the vendor repo.
    """

    @staticmethod
    def find_all_gn_build_variables_for_bazel(
        fuchsia_dir: Path, build_dir: Path
    ) -> list[str]:
        """Find `all gn_build_variables_for_bazel.json` files in the Fuchsia checkout.

        Args:
            fuchsia_dir: Path to Fuchsia source dir.
            build_dir: Path to Fuchsia build directory.

        Returns:
            A list of `gn_build_variables_for_bazel.json` file paths relative to
            the build directory root.

        Raises:
            ValueError if a required input file is missing.
        """
        args_files: list[str] = []

        base_file_name = "gn_build_variables_for_bazel.json"

        main_file_path = build_dir / base_file_name
        if not (main_file_path).exists():
            raise ValueError(
                f"Missing required build arguments file: {main_file_path}"
            )
        args_files.append(base_file_name)

        # The //vendor/ tree is optional and does not exist in open-source checkouts.
        vendor_dir = fuchsia_dir / "vendor"
        if vendor_dir.is_dir():
            for vendor_name in os.listdir(vendor_dir):
                vendor_file = f"vendor_{vendor_name}_{base_file_name}"
                if (build_dir / vendor_file).exists():
                    args_files.append(vendor_file)

        return args_files

    @staticmethod
    def generate_args_bzl(
        gn_args_to_export: list[dict[str, T.Any]], args_json_path: Path
    ) -> str:
        """Generate an `args.bzl` file defining values extracted from GN's args.

        Args:
            gn_args_to_export: A list of dictionaries describing each GN arg.
                See comments for //build/bazel:gn_build_variables_for_bazel for
                the format.
            args_json_path: Path of source file for build_args (never accessed).
        Returns:
            The content of the generated `args.bzl` file as a string.
        Raises:
            ValueError if the input content is malformed.
        """

        def fail(msg: str) -> None:
            raise ValueError(msg)

        args_contents = """# AUTO-GENERATED BY FUCHSIA BUILD - DO NOT EDIT
# Variables listed from {source_path}

\"\"\"A subset of GN args that are needed in the Bazel build.\"\"\"
""".format(
            source_path=args_json_path
        )

        for gn_arg in gn_args_to_export:
            args_contents += "\n# From {}\n".format(gn_arg["location"])
            varname = gn_arg["name"]
            vartype = gn_arg["type"]
            value = gn_arg["value"]
            if vartype in ["bool", "array_of_strings"]:
                args_contents += "{} = {}".format(varname, value)
            elif vartype in ["string", "string_or_false", "path"]:
                if vartype == "string_or_false" and not value:
                    value = ""
                elif vartype == "path":
                    if value.startswith("//"):
                        value = value[2:]
                    elif value.startswith("/"):
                        # Pass through the absolute path.
                        value = value
                    else:
                        fail(
                            "Path '{}' does not begin with '//' or '/'".format(
                                value,
                            )
                        )

                args_contents += '{} = "{}"'.format(varname, value)
            else:
                fail(
                    "Unknown type name '{}'".format(
                        vartype,
                    )
                )
            args_contents += "\n"

        return args_contents

    @staticmethod
    def record_fuchsia_build_config_dir(
        fuchsia_dir: Path,
        build_dir: Path,
        generated: GeneratedWorkspaceFiles,
    ) -> None:
        """Record the content of @fuchsia_build_info in a GeneratedWorkspaceFiles instance.

        Args:
            fuchsia_dir: Path to Fuchsia source directory.
            build_dir: Path to Fuchsia build directory.
            generated: A GeneratedWorkspaceFiles instance. Its record_file_content()
                method will be called to populate the repository.
        """
        generated.record_file_content("WORKSPACE.bazel", "")
        generated.record_file_content("BUILD.bazel", "")

        args_files_relative_paths = (
            GnBuildArgs.find_all_gn_build_variables_for_bazel(
                fuchsia_dir, build_dir
            )
        )

        for relative_path in sorted(args_files_relative_paths):
            file_path = build_dir / relative_path
            with (file_path).open("rb") as f:
                gn_args_json = json.load(f)
            args_bzl_content = GnBuildArgs.generate_args_bzl(
                gn_args_json, file_path
            )

            if relative_path.startswith("vendor_"):
                vendor_name = relative_path.split("_")[1]
                args_bzl_filename = f"vendor_{vendor_name}_args.bzl"
            else:
                args_bzl_filename = "args.bzl"
            generated.record_file_content(args_bzl_filename, args_bzl_content)

    @staticmethod
    def generate_fuchsia_build_info(
        fuchsia_dir: Path, build_dir: Path, repository_dir: Path
    ) -> None:
        generated = GeneratedWorkspaceFiles()
        GnBuildArgs.record_fuchsia_build_config_dir(
            fuchsia_dir, build_dir, generated
        )

        generated.update_if_needed(
            repository_dir, Path(f"{repository_dir}.generated-info.json")
        )


def record_gn_targets_dir(
    generated: GeneratedWorkspaceFiles,
    build_dir: Path,
    inputs_manifest_path: Path,
    all_licenses_spdx_path: Path,
) -> None:
    """Record the content of a @gn_targets directory in a GeneratedWorkspaceFiles instance.

    Args:
        generated: A GeneratedWorkspaceFiles instance.
        build_dir: Path to the Ninja build directory.
        inputs_manifest_path: Path to an inputs manifest file generated
            by the generate_gn_targets_repository_manifest() GN template.
            See //build/bazel/bazel_inputs.gni comments for file format.
        all_licenses_spdx_path: Path to an SPDX file listing all licensing
            requirements for the inputs covered by the manifest.
    Raises:
        ValueError in case of missing or malformed input.
    """
    # Ensure build_dir is absolute. Most symlink targets must be absolute for Bazel
    # to work properly.
    build_dir = build_dir.resolve()

    if not inputs_manifest_path.exists():
        raise ValueError(
            f"Missing inputs manifest file: {inputs_manifest_path}"
        )
    if not all_licenses_spdx_path.exists():
        raise ValueError(
            f"Missing licensing information file: {all_licenses_spdx_path}"
        )

    # This creates two sets of symlinks.
    #
    # The first one maps `_files/{ninja_path}` to the absolute path of the corresponding
    # Ninja artifact in the build directory, e.g.:
    #
    #  _files/obj/src/foo/foo.cc.o ----> $NINJA_BUILD_DIR/obj/src/foo/foo.cc
    #
    # There is one such symlink per Ninja output paths.
    #
    # Second, for each bazel_package value, `{bazel_package}/_files` will be a relative
    # symlink that points to the top-level `_files` directory, as in:
    #
    #  src/foo/_files ---> ../../_files
    #
    # This is used by the BUILD.bazel file generated in the same sub-directory, that can
    # reference the artifacts using labels like "_files/{ninja_path}" without having
    # to care for Bazel package boundaries, as in:
    #
    #  ```
    #  # Generated as src/foo/BUILD.bazel
    #  filegroup(
    #     name = "foo",
    #     srcs = [ "_files/obj/src/foo/foo.cc.o" ]
    #  )
    #  ```
    #
    # The reason why this double indirection exists is purely for debuggability!
    # It is easier to see all the Ninja artifacts exposed at once from the top-level
    # _files/ directory when verifying correctness.
    #
    # It is perfectly possible to only place absollute symlinks under
    # {bazel_package}/_files/... but doing this leads to repositories that are
    # harder to inspect in practice due to the extra long paths it creates.

    # The top-level directory that will contain symlinks to all Ninja output
    # files, using . For example _files/obj/src/foo/foo.cc.o
    build_dir_name = "_files"

    all_files = []

    # Build a { bazel_package -> { gn_target_name -> entry } } map.
    package_map: dict[str, dict[str, str]] = {}
    for entry in json.loads(generated.read_text_file(inputs_manifest_path)):
        bazel_package = entry["bazel_package"]
        bazel_name = entry["bazel_name"]
        name_map = package_map.setdefault(bazel_package, {})
        name_map[bazel_name] = entry

    # Create the ///{gn_dir}/BUILD.bazel file for each GN directory.
    # Every target defined in {gn_dir}/BUILD.gn that is part of the manifest
    # will have its own filegroup() entry with the corresponding target name.
    for bazel_package, name_map in package_map.items():
        content = """# AUTO-GENERATED - DO NOT EDIT

package(
    default_applicable_licenses = ["//:all_licenses_spdx_json"],
    default_visibility = ["//visibility:public"],
)

"""
        for bazel_name, entry in name_map.items():
            file_links = entry.get("output_files", [])
            if file_links:
                for file in file_links:
                    # Create //_files/{ninja_path} as a symlink to the Ninja output location.
                    generated.record_raw_symlink(
                        f"{build_dir_name}/{file}",
                        build_dir / file,
                    )
                    all_files.append(file)

                content += """
# From GN target: {label}
filegroup(
    name = "{name}",
    srcs = """.format(
                    label=entry["generator_label"], name=bazel_name
                )
                if len(file_links) == 1:
                    content += '["_files/%s"],\n' % file_links[0]
                else:
                    content += "[\n"
                    for file in file_links:
                        content += '        "_files/%s",\n' % file
                    content += "    ],\n"
                content += ")\n"

            dir_link = entry.get("output_directory", "")
            if dir_link:
                # Create //_files/{ninja_path} as a symlink to the real path.
                generated.record_raw_symlink(
                    f"{build_dir_name}/{dir_link}", build_dir / dir_link
                )

                content += """
# From GN target: {label}
filegroup(
    name = "{name}",
    srcs = glob(["{ninja_path}/**"], exclude_directories=1),
)
alias(
    name = "{name}.directory",
    actual = "{ninja_path}",
)
""".format(
                    label=entry["generator_label"],
                    name=bazel_name,
                    ninja_path=f"{build_dir_name}/{dir_link}",
                )

        generated.record_file_content(f"{bazel_package}/BUILD.bazel", content)

        # Because {bazel_package}/BUILD.bazel contains label references
        #
        # such as "_files/obj/src/tee/ta/noop/ta-noop.far", create
        # {bazel_package}/_files as a symlink to the top-level _files directory.
        #
        # A relative path target is required, as the final output directory path is
        # not known yet. This much walk back the bazel_package path fragments.
        generated.record_raw_symlink(
            f"{bazel_package}/{build_dir_name}",
            ("../" * len(bazel_package.split("/"))) + build_dir_name,
        )

    # The symlink for the special all_licenses_spdx.json file.
    # IMPORTANT: This must end in `.spdx.json` for license classification to work correctly!
    generated.record_symlink(
        "all_licenses.spdx.json", all_licenses_spdx_path.resolve()
    )

    # The content of BUILD.bazel
    build_content = """# AUTO-GENERATED - DO NOT EDIT
load("@rules_license//rules:license.bzl", "license")

# This contains information about all the licenses of all
# Ninja outputs exposed in this repository.
# IMPORTANT: package_name *must* be "Legacy Ninja Build Outputs"
# as several license pipeline exception files hard-code this under //vendor/...
license(
    name = "all_licenses_spdx_json",
    package_name = "Legacy Ninja Build Outputs",
    license_text = "all_licenses.spdx.json",
    visibility = ["//visibility:public"]
)

"""
    generated.record_file_content("BUILD.bazel", build_content)
    generated.record_file_content(
        "WORKSPACE.bazel", 'workspace(name = "gn_targets")\n'
    )
    generated.record_file_content(
        "MODULE.bazel", 'module(name = "gn_targets", version = "1")\n'
    )


def check_regenerator_inputs_updates(
    build_dir: Path,
    inputs_file: str = "regenerator_outputs/regenerator_inputs.txt",
) -> set[str]:
    """Check whether any regenerator input has changed.

    Args:
        build_dir: Ninja build directory path.
        inputs_file: Optional path string to the file listing all inputs,
           relative to build_dir. Default value is
           regenerator_outputs/regenerator_inputs.txt.

    Returns:
        A set of file paths, relative to build_dir, whose timestamp is
        newer than that of the inputs_file file itself.
    """
    inputs_path = build_dir / inputs_file
    if not inputs_path.exists():
        # If the file does not exist, a regeneration is required.
        return {inputs_file}

    inputs_timestamp = inputs_path.stat().st_mtime
    changed_inputs: set[str] = set()
    for dep in inputs_path.read_text().splitlines():
        dep_path = build_dir / dep
        if not dep_path.exists():
            changed_inputs.add(dep)
            continue

        dep_timestamp = dep_path.stat().st_mtime
        if dep_timestamp > inputs_timestamp:
            changed_inputs.add(dep)

    return changed_inputs


def repository_name(label: str) -> str:
    """Returns repository name of the input label.

    Supports both canonical repository names (starts with @@) and apparent
    repository names (starts with @).
    """
    repository, sep, _ = label.partition("//")
    assert sep == "//", f"Missing // in label: {label}"
    return repository.removeprefix("@@").removeprefix("@")


def innermost_repository_name(label: str) -> str:
    """Returns the innermost repository names.

    * For top-level repositories, this is their canonical repository name.

    * For repos generated by extensions, this is the repo_name used their
      corresponding extensions' repo namespaces. Repo generated by extensions
      have the canonical names in the form of
      `module_repo_canonical_name+extension_name+repo_name`, this function
      returns `repo_name` from it.

      See https://bazel.build/external/extension#repository_names_and_visibility

      NOTE: It's clearly stated in the doc that this naming convention is NOT an
      API. However, in our use cases, we'd either do rely on this or hardcode
      canonical names for extension generated repos everywhere. And the latter
      is even less reliable.
    """
    canonical_repo_name = repository_name(label)
    if _BAZEL_REPO_NAME_SEPARATOR not in canonical_repo_name:
        return canonical_repo_name
    elements = canonical_repo_name.split(_BAZEL_REPO_NAME_SEPARATOR)
    if len(elements) < 3:
        # This is a repo_name+version canonical name, return as-is.
        return canonical_repo_name
    return elements[-1]
