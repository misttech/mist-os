#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Convert an IDK prebuild manifest into an IDK.

Currently works on prebuild metadata for an IDK collection but generates files
that match the corresponding IDK for a single (unmerged) collection. See
https://fxbug.dev/408003238 for discussion of some differences.
"""

import argparse
import collections
import datetime
import difflib
import json
import os
import sys
import typing as T
from pathlib import Path

# Assume the script is in //build/sdk/generate_prebuild_idk/.
_SCRIPT_DIR = Path(__file__).parent
_FUCHSIA_ROOT_DIR = _SCRIPT_DIR.parent.parent.parent

# Import from this directory.
sys.path.insert(0, str(_SCRIPT_DIR))
import generate_sdk_package_manifest

# The directory that contains module dependencies for this script.
sys.path.insert(0, str(_SCRIPT_DIR / ".."))
# For yaml.
sys.path.insert(0, str(_FUCHSIA_ROOT_DIR / "third_party/pyyaml/src/lib"))

import generate_version_history
import yaml

# Information about an atom in the prebuild manifest.
AtomInfo: T.TypeAlias = dict[str, T.Any]

# The contents of a JSON file.
JsonFileContent: T.TypeAlias = dict[str, T.Any]

# Map an extra JSON file paths to their content.
JsonFilesMap: T.TypeAlias = dict[str, JsonFileContent]

# The contents of a meta.json file.
MetaJson: T.TypeAlias = JsonFileContent


def get_unique_sequence(seq: T.Sequence[T.Any]) -> T.List[T.Any]:
    """Remove duplicates from an input sequence, preserving order."""
    result = []
    visited = set()
    for item in seq:
        if not item in visited:
            result.append(item)
            visited.add(item)

    return result


def collect_directory_files(src_root: str | Path) -> T.List[str]:
    """Collect list of all files from a root directory.

    Args:
        src_root: Root directory path.
    Returns:
        A list of path strings pointing to all files in the root directory.
    """
    src_root = os.path.normpath(src_root)
    result = []
    for rootpath, _dirs, dir_files in os.walk(src_root):
        for file in dir_files:
            src_path = os.path.join(rootpath, file)
            result.append(os.path.relpath(src_path, src_root))
    return result


class DebugManifest(object):
    """Models an ELF debug manifest.

    The debug manifest is a text line where each line is <dest>=<src>
    where <dest> is a destination location relative to the SDK root,
    usually starting with `.build-id/`, and <src> is a location relative
    to the Ninja build directory where the matching unstripped ELF binary
    is.
    """

    def __init__(self, content: str) -> None:
        self._map: dict[str, str] = {}
        for line in content.splitlines():
            build_id_lib, sep, build_path = line.partition("=")
            assert (
                sep == "="
            ), f"Invalid debug manifest line (expected =): [{line}]"
            self._map[build_path] = build_id_lib

    def from_build_path(self, build_path: str) -> str:
        """Convert build path to unstripped library to final SDK location."""
        return self._map.get(build_path, "")

    @staticmethod
    def from_file(path: Path) -> "DebugManifest":
        """Create new instance from file path."""
        return DebugManifest(path.read_text())


class PrebuildMap(object):
    def __init__(self, prebuild_manifest: T.Sequence[AtomInfo]):
        self._labels_map: dict[str, AtomInfo] = {
            atom["atom_label"]: atom
            for atom in prebuild_manifest
            if atom["atom_type"] != "alias"
        }
        self._alias_map: dict[str, str] = {
            atom["atom_label"]: atom["atom_actual"]
            for atom in prebuild_manifest
            if atom["atom_type"] == "alias"
        }
        assert (len(self._labels_map) + len(self._alias_map)) == len(
            prebuild_manifest
        ), "Multiple atoms have the same `atom_label`."
        self._build_dir: T.Optional[Path] = None
        self._fuchsia_source_dir: T.Optional[Path] = None
        self._relative_source_prefix_from_build_dir = "../../"

    def set_build_dir(self, build_dir: Path) -> None:
        self._build_dir = build_dir.resolve()

    def set_fuchsia_source_dir(self, fuchsia_source_dir: Path) -> None:
        self._fuchsia_source_dir = fuchsia_source_dir.resolve()

    def _get_source_path(self, build_path: str) -> str:
        """Convert build directory path into source path if possible.

        Args:
            build_path: A path relative to the Ninja build directory.
        Returns:
            if the input path begins with
            self._relative_source_prefix_from_build_dir (e.g. '../../') then
            return its path relative to self._fuchsia_source_dir, otherwise this
            is a generated Ninja output, and return the empty string.
        """
        if build_path.startswith(self._relative_source_prefix_from_build_dir):
            return build_path.removeprefix(
                self._relative_source_prefix_from_build_dir
            )
        return ""

    def values(self) -> T.Sequence[AtomInfo]:
        return [*self._labels_map.values()]

    def items(self) -> T.Sequence[T.Tuple[str, AtomInfo]]:
        return [*self._labels_map.items()]

    def resolve_label(self, label: str) -> str:
        """Resolve a label through aliases."""
        return self._alias_map.get(label, label)

    def resolve_labels(self, labels: T.Sequence[str]) -> T.List[str]:
        """Resolve list of labels through aliases."""
        return [self.resolve_label(l) for l in labels]

    def resolve_unique_labels(self, labels: T.Sequence[str]) -> T.List[str]:
        """Resolve list of labels through aliases, removing duplicates."""
        return get_unique_sequence(self.resolve_labels(labels))

    def label_to_library_name(self, label: str) -> str:
        """Retrieve the library_name of a given atom label."""
        return self._labels_map[label]["prebuild_info"]["library_name"]

    def _deps_labels_to_atom_types(
        self, deps_labels: T.Sequence[str], atom_types: T.Sequence[str]
    ) -> T.List[str]:
        return sorted(
            self.label_to_library_name(d)
            for d in deps_labels
            if self._labels_map[d]["atom_type"] in atom_types
        )

    def labels_to_bind_library_names(
        self, deps_labels: T.Sequence[str]
    ) -> T.List[str]:
        """Convert a list of labels into a list of bind_library names."""
        return self._deps_labels_to_atom_types(deps_labels, ("bind_library",))

    def labels_to_fidl_library_names(
        self, deps_labels: T.Sequence[str]
    ) -> T.List[str]:
        """Convert a list of labels into a list of fidl_library names."""
        return self._deps_labels_to_atom_types(deps_labels, ("fidl_library",))

    def labels_to_cc_library_names(
        self, deps_labels: T.Sequence[str]
    ) -> T.List[str]:
        """Convert a list of labels into a list of cc_xxxx_library names."""
        return self._deps_labels_to_atom_types(
            deps_labels, ("cc_source_library", "cc_prebuilt_library")
        )

    def _is_valid_dependency_type(
        self, atom_type: str, dep_atom_type: str
    ) -> bool:
        """Checks if a single dependency relationship is valid.

        Args:
            atom_type: The type of the depending atom.
            dep_atom_type: The type of the dependency atom.

        Returns:
            True if the dependency is valid, False otherwise.
        """
        allowed_deps_types: list[str] = []
        match atom_type:
            case "cc_source_library":
                allowed_deps_types = [
                    "bind_library",
                    "cc_prebuilt_library",
                    "cc_source_library",
                    "fidl_library",
                    "none",
                ]
            case "cc_prebuilt_library":
                allowed_deps_types = [
                    "cc_prebuilt_library",
                    # TODO(https://fxbug.dev/42131085): verify that such
                    # libraries are header-only.
                    "cc_source_library",
                ]
            case "fidl_library":
                allowed_deps_types = ["fidl_library"]
            case "bind_library":
                allowed_deps_types = ["bind_library"]
            case _:
                assert False, f"Unexpected atom type with deps: {atom_type}"

        return dep_atom_type in allowed_deps_types

    def verify_dependency_relationships(self) -> int:
        """Verifies relationships between IDK atoms.

        Verifies atom dependencies are of allowed types.
        TODO(https://fxbug.dev/419105478): Add category validation.

        Returns:
            0 on success, a positive integer on failure.
        """
        for atom_info in self._labels_map.values():
            if atom_info["atom_type"] == "none":
                # A noop atom, such as "zircon_sdk" or a package that is not
                # supported in the current API level.
                continue
            if "prebuild_info" not in atom_info:
                # Atoms without prebuild info do not have deps.
                continue

            atom_type = atom_info["atom_type"]
            all_deps = self.resolve_unique_labels(
                atom_info["prebuild_info"].get("deps", {})
            )
            for dep_label in all_deps:
                dep_atom = self._labels_map[self.resolve_label(dep_label)]

                # Verify the atom type of the dependency is valid for the
                # current atom type.
                assert self._is_valid_dependency_type(
                    atom_type, dep_atom["atom_type"]
                ), (
                    "ERROR: '%s' atom '%s' has a dependency on '%s' of type '%s', which is not allowed."
                    % (
                        atom_type,
                        atom_info["atom_label"],
                        dep_label,
                        dep_atom["atom_type"],
                    )
                )

        return 0

    class GetMetaResult(T.NamedTuple):
        """The result of a call to `get_meta()`.

        Attributes:
            meta_json: The meta.json content or None if an unsupported type.
            additional_atom_files: A dictionary mapping additional additional
            atom files in the IDK to their sources.
            additional_json_files: A dictionary mapping additional JSON files in
            the IDK to their contents.
            additional_files_read: A set of additional files read.
        """

        meta_json: T.Optional[MetaJson]
        additional_atom_files: dict[str, str]
        additional_json_files: JsonFilesMap
        additional_files_read: set[str]

    def get_meta(self, info: AtomInfo) -> GetMetaResult:
        """Generate meta.json content for a given AtomInfo

        Returns a `GetMetaResult` object .
        """
        value = info["atom_meta"].get("value")
        if value is not None:
            # For reference, the following types are currently handled this way:
            #   "component_manifest"
            #   "config"
            #   "data"
            #   "documentation"
            #   "ffx_tool"
            #   "host_tool"
            #   "loadable_module"
            #   "sysroot"
            return self.GetMetaResult(value, {}, {}, set())

        generator = {
            "fidl_library": self._meta_for_fidl_library,
            "bind_library": self._meta_for_bind_library,
            "cc_prebuilt_library": self._meta_for_cc_prebuilt_library,
            "cc_source_library": self._meta_for_cc_source_library,
            "companion_host_tool": self._meta_for_companion_host_tool,
            "dart_library": self._meta_for_dart_library,
            "experimental_python_e2e_test": self._meta_for_experimental_python_e2e_test,
            "package": self._meta_for_package,
            "version_history": self._meta_for_version_history,
            "none": self._meta_for_noop,
            "collection": self._meta_for_collection,
        }.get(info["atom_type"], None)
        return (
            generator(info)
            if generator
            else self.GetMetaResult(None, {}, {}, set())
        )

    def _meta_for_fidl_library(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        fidl_sources = [f["dest"] for f in info["atom_files"]]
        fidl_deps = self.resolve_unique_labels(prebuild.get("deps", {}))
        return self.GetMetaResult(
            {
                "name": prebuild["library_name"],
                "root": prebuild["file_base"],
                "sources": fidl_sources,
                "stable": info["is_stable"],
                "type": info["atom_type"],
                "deps": [self.label_to_library_name(d) for d in fidl_deps],
            },
            {},
            {},
            set(),
        )

    def _meta_for_bind_library(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        bind_sources = [f["dest"] for f in info["atom_files"]]
        bind_deps = self.resolve_unique_labels(prebuild.get("deps", {}))
        return self.GetMetaResult(
            {
                "name": prebuild["library_name"],
                "root": prebuild["file_base"],
                "deps": [self.label_to_library_name(d) for d in bind_deps],
                "sources": bind_sources,
                "type": info["atom_type"],
            },
            {},
            {},
            set(),
        )

    def _meta_for_cc_source_library(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        all_deps = self.resolve_unique_labels(prebuild.get("deps", {}))

        fidl_deps = []
        fidl_layers = collections.defaultdict(list)
        for dep_label in get_unique_sequence(prebuild.get("deps", {})):
            dep_atom = self._labels_map[self.resolve_label(dep_label)]
            if dep_atom["atom_type"] != "fidl_library":
                continue

            name = dep_atom["prebuild_info"]["library_name"]
            dep_label = dep_label.removesuffix("_sdk")
            if "_cpp" in dep_label:
                layer = "cpp" + dep_label.split("_cpp", maxsplit=1)[1]
                fidl_layers[layer].append(name)
            else:
                # There was no suffix, so this is either non-cpp binding dep or it's an hlcpp dep.
                # this covers both of those bases.
                fidl_layers["hlcpp"].append(name)
                fidl_deps.append(name)

        return self.GetMetaResult(
            {
                "name": prebuild["library_name"],
                "root": prebuild["file_base"],
                "deps": self.labels_to_cc_library_names(all_deps),
                "bind_deps": self.labels_to_bind_library_names(all_deps),
                "fidl_deps": fidl_deps,
                "fidl_binding_deps": [
                    {"binding_type": layer, "deps": sorted(set(dep))}
                    for layer, dep in fidl_layers.items()
                ],
                "headers": prebuild["headers"],
                "include_dir": prebuild["include_dir"],
                "sources": prebuild["sources"],
                "stable": info["is_stable"],
                "type": info["atom_type"],
            },
            {},
            {},
            set(),
        )

    def _meta_for_cc_prebuilt_library(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        binaries = {}
        variants = []

        binary = prebuild["binaries"]
        arch = binary["arch"]
        api_level = binary["api_level"]
        dist_lib = binary.get("dist_lib")
        dist_path = binary.get("dist_path")
        link_lib = binary["link_lib"]
        debug_lib = binary.get("debug_lib", None)
        ifs_file = binary.get("ifs", None)

        # TODO(https://fxbug.dev/310006516): Remove the `if` block when the
        # `arch/` directory is removed from the IDK.
        if api_level == "PLATFORM":
            binaries[arch] = {
                "link": link_lib,
            }
            if dist_lib:
                binaries[arch]["dist"] = dist_lib
                binaries[arch]["dist_path"] = dist_path
            if debug_lib:
                binaries[arch]["debug"] = debug_lib
        else:
            variant = {
                "constraints": {
                    "api_level": api_level,
                    "arch": arch,
                },
                "values": {
                    "link_lib": link_lib,
                },
            }
            if dist_lib:
                variant["values"]["dist_lib"] = dist_lib
                variant["values"]["dist_lib_dest"] = dist_path
            if debug_lib:
                variant["values"]["debug"] = debug_lib
            if ifs_file:
                variant["values"]["ifs"] = ifs_file
            variants.append(variant)

        all_deps = self.resolve_unique_labels(prebuild.get("deps", {}))
        result = {
            "name": prebuild["library_name"],
            "root": prebuild["file_base"],
            "format": prebuild["format"],
            "headers": prebuild["headers"],
            "include_dir": prebuild["include_dir"],
            "type": info["atom_type"],
            "deps": self.labels_to_cc_library_names(all_deps),
        }
        if binaries:
            result["binaries"] = binaries
            if ifs_file:
                result["ifs"] = ifs_file
        if variants:
            result["variants"] = variants
        return self.GetMetaResult(result, {}, {}, set())

    def _meta_for_version_history(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        # prebuild contains enough information to generate the final version
        # history file  by calling a Python module function.

        additional_files_read: set[str] = set()

        additional_files_read.add(prebuild["source"])
        with (self._build_dir / prebuild["source"]).open() as f:
            version_history = json.load(f)

        additional_files_read.add(prebuild["daily_commit_hash_file"])
        daily_commit_hash_file_path = (
            self._build_dir / prebuild["daily_commit_hash_file"]
        )
        with daily_commit_hash_file_path.open() as f:
            daily_commit_hash = f.read().strip()

        additional_files_read.add(prebuild["daily_commit_timestamp_file"])
        daily_commit_timestamp_file_path = (
            self._build_dir / prebuild["daily_commit_timestamp_file"]
        )
        with daily_commit_timestamp_file_path.open() as f:
            daily_commit_timestamp = datetime.datetime.fromtimestamp(
                int(f.read().strip()), datetime.UTC
            )
        generate_version_history.replace_special_abi_revisions(
            version_history, daily_commit_hash, daily_commit_timestamp
        )

        # TODO(https://fxbug.dev/383361369): Delete this once all clients have
        # been updated to use "phase" and it is removed from the real instance.
        generate_version_history.add_deprecated_status_field(version_history)

        return self.GetMetaResult(
            version_history, {}, {}, additional_files_read
        )

    def _meta_for_companion_host_tool(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        result = {
            "name": prebuild["name"],
            "root": "tools",
            "type": "companion_host_tool",
        }
        src_root = prebuild["src_root"]
        dest_root = prebuild["dest_root"]

        src_dir = self._get_source_path(src_root)

        binary_relpath = os.path.relpath(prebuild["binary"], src_root)
        files = [os.path.join(dest_root, binary_relpath)]
        additional_atom_files: dict[str, str] = {}

        prebuilt_files = None
        if "prebuilt_files" in prebuild:
            prebuilt_files = prebuild["prebuilt_files"]
        else:
            assert src_dir
            assert self._fuchsia_source_dir
            prebuilt_files = collect_directory_files(
                self._fuchsia_source_dir / src_dir
            )

        assert prebuilt_files
        for prebuilt_file in prebuilt_files:
            source_path = os.path.join(src_root, prebuilt_file)
            dest_path = os.path.join(dest_root, prebuilt_file)
            files.append(dest_path)
            additional_atom_files[dest_path] = source_path

        # Remove duplicates if any.
        files = get_unique_sequence(files)

        # Sort all files except the first one, which must be the binary.
        result["files"] = [files[0]] + sorted(files[1:])

        return self.GetMetaResult(result, additional_atom_files, {}, set())

    def _meta_for_dart_library(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]

        # The list of packages that should be pulled from a Flutter SDK instead of pub.
        FLUTTER_PACKAGES = [
            "flutter",
            "flutter_driver",
            "flutter_test",
            "flutter_tools",
        ]

        additional_files_read: set[str] = set()
        third_party_deps: list[object] = []
        for spec_file in prebuild["third_party_specs"]:
            spec_file_path = os.path.normpath(self._build_dir / spec_file)
            additional_files_read.add(str(spec_file))
            with open(spec_file_path) as spec_f:
                manifest = yaml.safe_load(spec_f)
                name = manifest["name"]
                dep = {
                    "name": name,
                }
                if name in FLUTTER_PACKAGES:
                    dep["version"] = "flutter_sdk"
                else:
                    if "version" not in manifest:
                        raise Exception(
                            "%s does not specify a version." % spec_file
                        )
                    dep["version"] = manifest["version"]
                third_party_deps.append(dep)

        dart_deps = []
        fidl_deps = []
        for dep_label in prebuild["deps"]:
            dep_label = self.resolve_label(dep_label)
            dep_info = self._labels_map[dep_label]
            if dep_info["atom_type"] == "dart_library":
                dep_name = dep_info["prebuild"]["library_name"]
                dart_deps.append(dep_name)
            elif dep_info["atom_type"] == "fidl_library":
                dep_name = dep_info["prebuild"]["library_name"]
                fidl_deps.append(dep_name)

        result = {
            "type": "dart_library",
            "name": prebuild["library_name"],
            "root": prebuild["file_base"],
            "sources": prebuild["sources"],
            "deps": dart_deps,
            "fidl_deps": fidl_deps,
            "third_party_deps": third_party_deps,
        }
        if prebuild["null_safe"]:
            result["dart_library_null_safe"] = True
        return self.GetMetaResult(result, {}, {}, additional_files_read)

    def _meta_for_experimental_python_e2e_test(
        self, info: AtomInfo
    ) -> GetMetaResult:
        prebuild = info["prebuild_info"]

        root = prebuild["file_base"]
        api_level = prebuild["api_level"]
        versioned_root = f"{root}/{api_level}"

        files: T.List[str] = []

        def get_gn_generated_path(path: str) -> str:
            if not path.startswith("GN_GENERATED("):
                return ""
            assert path[-1] == ")", f"Invalid GN generated path: {path}"
            return path.removeprefix("GN_GENERATED(")[:-1]

        test_sources_list = get_gn_generated_path(prebuild["test_sources_list"])
        assert (
            test_sources_list
        ), f'Invalid test_sources_list value: {prebuild["test_sources_list"]}'

        # Process required test sources from file.
        assert self._build_dir
        additional_atom_files: dict[str, str] = {}
        additional_files_read: set[str] = set()
        additional_files_read.add(test_sources_list)
        with (self._build_dir / test_sources_list).open() as f:
            test_sources_json = json.load(f)
            for entry in test_sources_json:
                dest_path = f"{versioned_root}/{entry['name']}"
                source_path = entry["path"]
                files.append(f"{dest_path}={source_path}")
                additional_atom_files[dest_path] = source_path

        return self.GetMetaResult(
            {
                "name": prebuild["name"],
                "root": root,
                "type": info["atom_type"],
                "files": [f.split("=")[0] for f in files],
            },
            additional_atom_files,
            {},
            additional_files_read,
        )

    def _meta_for_package(self, info: AtomInfo) -> GetMetaResult:
        """Generates metadata for Fuchsia package atoms.

        Unlike peer functions, the metadata for packages can only be generated
        after the Ninja build because the blob IDs are not predictable.
        """
        prebuild = info["prebuild_info"]

        package_manifest_relative_path = prebuild["package_manifest"]
        api_level = prebuild["api_level"]
        arch = prebuild["arch"]
        distribution_name = prebuild["distribution_name"]

        # The base metadata. The function will add to it.
        meta = {
            "name": distribution_name,
            "variants": [],
            "type": "package",
        }

        # A structure defined by the function.
        inputs = {
            "api_level": api_level,
            "arch": arch,
            "distribution_name": distribution_name,
        }

        # Mapping of destinations in the IDK to source files.
        sdk_file_map: dict[str, str] = {}

        # This function must rewrite the content of the underlying package
        # manifest. As a result, unlike other files that are simply symlinks,
        # new files must be created. Since we don't know where to write them,
        # return the file paths and contents to be written along with other meta
        # files.
        (
            _,
            additional_meta_files,
        ) = generate_sdk_package_manifest.handle_package_manifest(
            None,
            self._build_dir / package_manifest_relative_path,
            sdk_file_map,
            meta,
            None,
            inputs,
        )

        assert (
            len(additional_meta_files.items()) == 1
        ), "Add a test for subpackage support before adding subpackages to the IDK."

        additional_atom_files: dict[str, str] = {}
        for dest, source in sdk_file_map.items():
            additional_atom_files[dest] = source

        additional_files_read: set[str] = set()
        additional_files_read.add(package_manifest_relative_path)

        return self.GetMetaResult(
            meta,
            additional_atom_files,
            additional_meta_files,
            additional_files_read,
        )

    def _meta_for_noop(self, info: AtomInfo) -> GetMetaResult:
        return self.GetMetaResult(None, {}, {}, set())

    def _meta_for_collection(self, info: AtomInfo) -> GetMetaResult:
        prebuild = info["prebuild_info"]
        return self.GetMetaResult(
            {
                "arch": prebuild["arch"],
                "id": info.get("atom_id", ""),
                "parts": list[dict[str, T.Any]],
                "root": prebuild["root"],
                "schema_version": prebuild["schema_version"],
            },
            {},
            {},
            set(),
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--prebuild-manifest",
        type=Path,
        required=True,
        help="Path to the input prebuilt manifest JSON file (generated by GN gen).",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help=(
            "Path to the directory in which to write the IDK. "
            "A new directory will be created at this location."
            "Any existing directory will be deleted."
        ),
    )
    parser.add_argument(
        "--fuchsia-source-dir",
        type=Path,
        help="Specify the Fuchsia source directory.",
    )
    parser.add_argument(
        "--build-dir",
        type=Path,
        default=Path("out/default"),
        help="Specify the Ninja build output directory.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help=(
            "Verify the content of the generated files against regular ones. "
            "Does not verify whether the file should be in the IDK."
        ),
    )
    parser.add_argument(
        "--stamp-file",
        help="Path to the stamp file. Required if `--depfile` is specified.",
        type=Path,
        required=False,
    )
    parser.add_argument(
        "--depfile",
        help="Path to the depfile.",
        type=Path,
        required=False,
    )
    args = parser.parse_args()

    with args.prebuild_manifest.open() as f:
        prebuild_manifest = json.load(f)

    generator = IdkGenerator(
        prebuild_manifest, args.build_dir, args.fuchsia_source_dir
    )

    additional_files_read: set[str] = set()
    additional_written_files: set[str] = set()

    result, additional_files_read = generator.GenerateMetaFileContents()
    if result != 0:
        return result

    if args.check:
        result = generator.VerifyMetaFileContentsAgainstNinjaGeneratedFiles()
        if result != 0:
            return result

    if args.output_dir:
        (
            result,
            additional_written_files,
        ) = generator.WriteIdkContentsToDirectory(args.output_dir)
        if result != 0:
            return result

    if args.stamp_file:
        args.stamp_file.touch()

    if args.depfile:
        depfile_path: Path = args.depfile
        depfile_path.parent.mkdir(parents=True, exist_ok=True)
        with open(args.depfile, "w") as depfile:
            all_dependencies = additional_files_read
            all_dependencies.add(str(args.prebuild_manifest))
            depfile.write(
                "{} {}: {}\n".format(
                    args.stamp_file,
                    " ".join(additional_written_files),
                    " ".join(all_dependencies),
                )
            )

    return 0


class IdkGenerator(object):
    def __init__(
        self,
        prebuild_manifest: T.Sequence[AtomInfo],
        build_dir: Path,
        fuchsia_source_dir: T.Optional[Path],
    ):
        self.collection_meta_path: str | None = None

        # Map of meta.json destination paths to their JSON contents.
        self._meta_files: dict[str, MetaJson] = {}

        # Map of other JSON file destination paths to their JSON contents.
        self._json_files: JsonFilesMap = {}

        # Map of destination files to their source. Symlinks should be created
        # for each mapping. Each entry must be unique.
        self._atom_files: dict[str, str] = {}

        # Map of destination files to their source specifically for
        # package atoms. Symlinks should  be created for each mapping. Each
        # entry must be unique, but this must be managed since packages can
        # specify the same blobs.
        self._package_atom_files: dict[str, str] = {}

        self._build_dir = build_dir

        if not fuchsia_source_dir:
            # Assume this script is under //build/sdk/generate_prebuild_idk/.
            fuchsia_source_dir = Path(__file__).parent.parent.parent.parent

        self._prebuild_map = PrebuildMap(prebuild_manifest)
        self._prebuild_map.set_fuchsia_source_dir(fuchsia_source_dir)
        self._prebuild_map.set_build_dir(build_dir)

    def GenerateMetaFileContents(self) -> tuple[int, set[str]]:
        """Processes the data in `self._prebuild_map`.

        Populates `self._meta_files` with the contents of meta files and
        `self._atom_files` and `self._package_atom_files` with files to be
        copied.

        Must be called before other methods and may only be called one time.

        Returns:
            A tuple containing the return code (0 upon success and a positive
            integer otherwise) and a list of additional files read.
        """
        assert (
            not self._meta_files
            and not self._atom_files
            and not self._package_atom_files
        )

        result = self._prebuild_map.verify_dependency_relationships()
        if result != 0:
            return result, set()

        unhandled_labels = set()
        collection_parts: list[dict[str, T.Any]] = []
        all_additional_files_read: set[str] = set()

        for info in self._prebuild_map.values():
            # Note: Due to the way the prebuild manifest is currently generated,
            # any atom in the "partner" category that is a dependency of the IDK
            # collection will be included in the IDK, even if it is an indirect
            # dependency, such as within a prebuilt library.
            # The IDK manifest golden build tests ensure any new IDK atoms that
            # may result from this are caught.
            if info["category"] != "partner":
                continue

            (
                meta_json,
                additional_atom_files,
                additional_json_files,
                additional_files_read,
            ) = self._prebuild_map.get_meta(info)
            assert meta_json or (
                not additional_atom_files
                and not additional_json_files
                and not additional_files_read
            )
            if meta_json:
                meta_path = info["atom_meta"]["dest"]
                self._meta_files[meta_path] = meta_json

                if info["atom_type"] == "collection":
                    assert (
                        not self.collection_meta_path
                    ), "More than one collection info provided."
                    self.collection_meta_path = meta_path
                else:
                    collection_parts.append(
                        {
                            "meta": meta_path,
                            "stable": info["is_stable"],
                            "type": info["atom_type"],
                        }
                    )
                    for file in info["atom_files"]:
                        dest = file["dest"]
                        source = file["source"]
                        assert (
                            dest not in self._atom_files
                        ), f"Path `{dest}` specified by multiple atoms."
                        self._atom_files[dest] = source

                if additional_atom_files:
                    if info["atom_type"] == "package":
                        # Packages can have the same blobs as other packages.
                        # Avoid adding duplicates while ensuring the source
                        # path is identical.
                        for dest, source in additional_atom_files.items():
                            assert (
                                dest not in self._atom_files
                            ), f"Path `{dest}` specified by non-package atoms."
                            if dest in self._package_atom_files.keys():
                                assert self._package_atom_files[dest] == source
                            else:
                                self._package_atom_files[dest] = source
                    else:
                        for dest in additional_atom_files.keys():
                            assert (
                                dest not in self._atom_files
                            ), f"Path `{dest}` specified by multiple atoms."
                        self._atom_files.update(additional_atom_files)

                if additional_json_files:
                    self._json_files.update(additional_json_files)
                if additional_files_read:
                    all_additional_files_read |= additional_files_read
            elif info["atom_type"] != "none":
                unhandled_labels.add(info["atom_label"])

        assert (
            not unhandled_labels
        ), "ERROR: Unhandled labels:\n%s\n" % "\n".join(
            sorted(unhandled_labels)
        )

        collection_parts.sort(key=lambda a: (a["meta"], a["type"]))

        # Populate the IDK manifest.
        assert self.collection_meta_path, "Collection info must be provided."
        self._meta_files[self.collection_meta_path]["parts"] = collection_parts
        return 0, all_additional_files_read

    def VerifyMetaFileContentsAgainstNinjaGeneratedFiles(self) -> int:
        """Verifies the meta file content generated from prebuild data against
        the files generated by the Ninja build.

        The IDK collection corresponding to the prebuild manifest must have been
        successfully built since any changes were made.

        The reference files are identified by the "atom_meta_json_file" fields
        in the prebuild metadata.

        Returns:
            0 when no differences are found and a positive integer otherwise.
        """
        assert self._meta_files

        failed = False
        for atom_label, info in self._prebuild_map.items():
            meta_path = info["atom_meta"]["dest"]
            meta_json = self._meta_files.get(meta_path)
            if meta_json is None:
                continue

            meta_json_content = json.dumps(meta_json, sort_keys=True, indent=2)

            reference_json = info["atom_meta_json_file"]
            reference_path = Path(self._build_dir / reference_json)
            if not reference_path.exists():
                print(
                    f"ERROR: Missing build file: {reference_path}",
                    file=sys.stderr,
                )
                continue

            # Some of the meta.json in the Ninja build directory are written by
            # GN directly through generated_file() and are not formatted properly,
            # so reformat them to compare their content.
            reference_content = reference_path.read_text()
            reference_content = json.dumps(
                json.loads(reference_content), sort_keys=True, indent=2
            )

            # Do the comparison, print differences.
            if meta_json_content != reference_content:
                failed = True
                print(
                    f"ERROR: meta.json differences for {atom_label}:",
                    file=sys.stderr,
                )
                print(
                    "\n".join(
                        difflib.unified_diff(
                            reference_content.splitlines(),
                            meta_json_content.splitlines(),
                            fromfile=f"from_ninja {reference_path}",
                            tofile="from_prebuild",
                            lineterm="",
                        )
                    ),
                    file=sys.stderr,
                )
                print("", file=sys.stderr)
        if failed:
            # Error message(s) were printed above.
            return 1
        else:
            print(
                "PASS: All generated metadata files have the same contents as "
                "the corresponding Ninja-built files."
            )
            return 0

    def WriteIdkContentsToDirectory(
        self, output_dir: Path
    ) -> tuple[int, set[str]]:
        """Writes the generated contents to `output_dir`.

        Writes the metadata in `self._meta_files` to files, creates symlinks
        for each `self._atom_files` and `self._package_atom_files`, and writes
        each `self._json_files`.

        Args:
            output_dir: Path to the directory in which to write the IDK.
            A new directory will be created at this location.
            Any existing directory will be deleted.
        Returns:

            A tuple containing the return code (0 upon success and a positive
            integer otherwise) and a list of paths written in addition to the
            collection manifest.
        """
        assert self._meta_files

        # Remove any existing outputs. Manually removing all subdirectories and
        # files instead of using shutil.rmtree, to avoid registering spurious
        # reads on stale subdirectories.
        if output_dir.exists():
            for root, dirs, files in os.walk(output_dir, topdown=False):
                for file in files:
                    os.unlink(os.path.join(root, file))
                for dir in dirs:
                    os.rmdir(os.path.join(root, dir))

        additional_written_files: set[str] = set()

        for meta_path, meta_json in self._meta_files.items():
            dest_path = output_dir / meta_path

            # The collection manifest path is known and does not need to be
            # provided. Also, Ninja reports a cyclic dependency if the same file
            # is listed in GN outputs and the depfile.
            assert self.collection_meta_path
            if meta_path != self.collection_meta_path:
                additional_written_files.add(str(dest_path))

            dest_path.parent.mkdir(parents=True, exist_ok=True)
            with dest_path.open("w") as f:
                json.dump(meta_json, f, sort_keys=True, indent=2)

        # Create symlinks for all atom files, including those for packages.
        all_atom_files = {**self._atom_files, **self._package_atom_files}
        for dest, source in all_atom_files.items():
            target_path = self._build_dir / source
            # The target directory must exist even if the file does not.
            target_path.parent.mkdir(parents=True, exist_ok=True)
            dest_path = output_dir / dest
            dest_path.parent.mkdir(parents=True, exist_ok=True)

            # Symlinks do not count as file writes, so `dest_path` does not need
            # to be added to `additional_written_files`.`
            os.symlink(target_path, dest_path)

        for meta_path, json_contents in self._json_files.items():
            dest_path = output_dir / meta_path
            additional_written_files.add(str(dest_path))

            dest_path.parent.mkdir(parents=True, exist_ok=True)
            with dest_path.open("w") as f:
                json.dump(json_contents, f, sort_keys=True, indent=2)

        return 0, additional_written_files


if __name__ == "__main__":
    sys.exit(main())
