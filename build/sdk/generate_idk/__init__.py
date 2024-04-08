#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functionality for building the IDK from its component parts."""

# See https://stackoverflow.com/questions/33533148/how-do-i-type-hint-a-method-with-the-type-of-the-enclosing-class
from __future__ import annotations

import dataclasses
import filecmp
import json
import itertools
import pathlib
from typing import Any, Callable, Sequence, TypeVar, Literal, Mapping, Optional

from typing import TypedDict

# version_history.json doesn't follow the same schema as other IDK metadata
# files, so we treat it specially in a few places.
VERSION_HISTORY_PATH = pathlib.Path("version_history.json")


class BuildManifestJson(TypedDict):
    """A type description of a subset of the fields in a build manifest.

    We don't explicitly check that the manifests in question actually match this
    schema - we just assume it.
    """

    atoms: list[ManifestAtom]


class ManifestAtom(TypedDict):
    type: str
    meta: str
    files: list[AtomFile]


class AtomFile(TypedDict):
    source: str
    destination: str


# The next few types describe a subset of the fields of various atom manifests,
# as they will be included in the IDK.


class CCPrebuiltLibraryMeta(TypedDict):
    name: str
    type: Literal["cc_prebuilt_library"]
    binaries: dict[str, Any]
    variants: list[Any]


class SysrootMeta(TypedDict):
    name: str
    type: Literal["sysroot"]
    versions: dict[str, Any]
    variants: list[Any]


class PackageMeta(TypedDict):
    name: str
    type: Literal["package"]
    variants: list[Any]


class LoadableModuleMeta(TypedDict):
    name: str
    type: Literal["loadable_module"]
    binaries: dict[str, Any]


class UnmergableMeta(TypedDict):
    name: str
    type: (
        Literal["cc_source_library"]
        | Literal["dart_library"]
        | Literal["fidl_library"]
        | Literal["documentation"]
        | Literal["device_profile"]
        | Literal["config"]
        | Literal["license"]
        | Literal["component_manifest"]
        | Literal["bind_library"]
        | Literal["version_history"]
        | Literal["experimental_python_e2e_test"]
    )


AtomMeta = (
    CCPrebuiltLibraryMeta
    | LoadableModuleMeta
    | PackageMeta
    | SysrootMeta
    | UnmergableMeta
)


class AtomMergeError(Exception):
    def __init__(self, atom_path: pathlib.Path):
        super(AtomMergeError, self).__init__(
            "While merging atom: %s" % atom_path
        )


@dataclasses.dataclass
class PartialIDK:
    """A model of a (potentially incomplete) IDK.

    Attributes:
        atoms (dict[pathlib.Path, AtomMeta]): Atoms to include in the IDK,
            indexed by the path to their metadata file, relative to the final
            IDK directory (e.g., `bind/fuchsia.ethernet/meta.json`). The values
            are the parsed JSON objects that will be written to that path.
        dest_to_src (dict[pathlib.Path, pathlib.Path]): All non-metadata files
            in the IDK belong in this dictionary. The key is the file path
            relative to the final IDK directory. The value is either absolute or
            relative to the current working directory.
    """

    atoms: dict[pathlib.Path, AtomMeta]
    dest_to_src: dict[pathlib.Path, pathlib.Path]

    @staticmethod
    def load(
        build_dir: pathlib.Path, relative_manifest_path: pathlib.Path
    ) -> PartialIDK:
        """Load relevant information about a piece of the IDK from a subbuild
        dir."""
        with (build_dir / relative_manifest_path).open() as f:
            build_manifest: BuildManifestJson = json.load(f)

        result = PartialIDK(atoms={}, dest_to_src={})
        for atom in build_manifest["atoms"]:
            for file in atom["files"]:
                src_path = build_dir / file["source"]
                dest_path = pathlib.Path(file["destination"])

                # Determine if this file is the metadata file for this atom.
                if atom["meta"] == file["destination"]:
                    with src_path.open() as f:
                        assert dest_path not in result.atoms, (
                            "Atom metadata file specified multiple times: %s"
                            % dest_path
                        )
                        result.atoms[dest_path] = json.load(f)
                else:
                    # Some files may be listed under multiple atoms, for
                    # example, package blobs. That's fine, but they must always
                    # have the same source file.
                    if prev_src := result.dest_to_src.get(dest_path):
                        assert src_path.samefile(prev_src), (
                            "IDK file '%s' listed with multiple sources:\n- %s\n- %s"
                            % (dest_path, prev_src, src_path)
                        )
                    else:
                        result.dest_to_src[dest_path] = src_path

        return result

    def merge_with(self, other: PartialIDK) -> PartialIDK:
        """Merge the contents of this PartialIDK with another.

        Put enough of them together, and you get a full IDK!
        """
        return PartialIDK(
            atoms=_merge_atoms(self.atoms, other.atoms),
            dest_to_src=_merge_other_files(self.dest_to_src, other.dest_to_src),
        )

    def sdk_manifest_json(
        self, host_arch: str, target_arch: list[str], release_version: str
    ) -> Any:
        """Returns the contents of manifest.json to include in the IDK.

        Note that this *isn't* the same as the "build manifest" that's referred
        to elsewhere in this file. This is the manifest that's actually included
        in the IDK itself at `meta/manifest.json`."""
        index = []
        for meta_path, atom in self.atoms.items():
            # Some atoms are given different "types" in the overall manifest...
            if meta_path == VERSION_HISTORY_PATH:
                type = "version_history"
            elif atom["type"] in ["component_manifest", "config"]:
                type = "data"
            else:
                type = atom["type"]
            index.append(dict(meta=str(meta_path), type=type))

        index.sort(key=lambda a: (a["meta"], a["type"]))

        return {
            "arch": {
                "host": host_arch,
                "target": target_arch,
            },
            "id": release_version,
            "parts": index,
            "root": "..",
            "schema_version": "1",
        }


def _merge_atoms(
    a: dict[pathlib.Path, AtomMeta], b: dict[pathlib.Path, AtomMeta]
) -> dict[pathlib.Path, AtomMeta]:
    """Merge two dictionaries full of atoms."""
    result = {}

    all_atoms = set([*a.keys(), *b.keys()])
    for atom_path in all_atoms:
        atom_a = a.get(atom_path)
        atom_b = b.get(atom_path)

        if atom_a and atom_b:
            if atom_path == VERSION_HISTORY_PATH:
                # Treat version_history.json specially, since it doesn't have a
                # 'type' field.
                assert (
                    atom_a == atom_b
                ), "A and B had different 'version_history' values. Huh?"
                result[atom_path] = atom_a
            else:
                # Merge atoms found in both IDKs.
                try:
                    result[atom_path] = _merge_atom(atom_a, atom_b)
                except Exception as e:
                    raise AtomMergeError(atom_path) from e
        elif atom_a:
            result[atom_path] = atom_a
        else:
            assert atom_b, "unreachable. Atom '%s' had falsy value?" % atom_path
            result[atom_path] = atom_b
    return result


def _merge_other_files(
    a: dict[pathlib.Path, pathlib.Path], b: dict[pathlib.Path, pathlib.Path]
) -> dict[pathlib.Path, pathlib.Path]:
    """Merge two dictionaries from (destination -> src). Shared keys are only
    allowed if the value files have the same contents."""
    result = {}

    all_files = set([*a.keys(), *b.keys()])
    for dest in all_files:
        src_a = a.get(dest)
        src_b = b.get(dest)
        if src_a and src_b:
            # Unfortunately, sometimes two separate subbuilds provide the same
            # destination file (particularly, blobs within packages). We have to
            # support this, but make sure that the file contents are identical.

            # Only inspect the files if the paths differ. This way we don't need
            # to go to disk in all the tests.
            if src_a != src_b:
                assert filecmp.cmp(src_a, src_b, shallow=False), (
                    "Multiple non-identical files want to be written to %s:\n- %s\n- %s"
                    % (
                        dest,
                        src_a,
                        src_b,
                    )
                )
            result[dest] = src_a
        elif src_a:
            result[dest] = src_a
        else:
            assert src_b, "unreachable. File '%s' had falsy source?" % dest
            result[dest] = src_b

    return result


def _assert_dicts_equal(
    a: Mapping[str, Any], b: Mapping[str, Any], ignore_keys: list[str]
) -> None:
    """Assert that the given dictionaries are equal on all keys not listed in
    `ignore_keys`."""
    keys_to_compare = set([*a.keys(), *b.keys()]) - set(ignore_keys)
    for key in keys_to_compare:
        assert a.get(key) == b.get(
            key
        ), "Key '%s' does not match. a[%s] = %s; b[%s] = %s" % (
            key,
            key,
            a.get(key),
            key,
            b.get(key),
        )


T = TypeVar("T")
K = TypeVar("K")


def _merge_unique_variants(
    vs1: Optional[Sequence[T]],
    vs2: Optional[Sequence[T]],
    dedup_key: Callable[[T], K],
) -> list[T]:
    """Merge vs1 and vs2, and assert that all values are all unique when
    projected through `dedup_key`. If either argument is None, it is treated as
    if it was empty."""

    result = [*(vs1 or []), *(vs2 or [])]
    # For all pairs...
    for v1, v2 in itertools.combinations(result, 2):
        assert dedup_key(v1) != dedup_key(
            v2
        ), "found duplicate variants:\n- %s\n- %s" % (v1, v2)
    return result


def _merge_disjoint_dicts(
    a: Optional[dict[str, Any]], b: Optional[dict[str, Any]]
) -> dict[str, Any]:
    """Merge two dicts, asserting that they have no overlapping keys. If either
    dict is None, it is treated as if it was empty."""
    if a and b:
        assert a.keys().isdisjoint(
            b.keys()
        ), "a and b have overlapping keys: %s vs %s" % (
            a.keys(),
            b.keys(),
        )
        return {**a, **b}
    else:
        return a or b or {}


def _merge_atom(a: AtomMeta, b: AtomMeta) -> AtomMeta:
    """Merge two atoms, according to type-specific rules."""
    if a["type"] in (
        "cc_source_library",
        "dart_library",
        "fidl_library",
        "documentation",
        "device_profile",
        "config",
        "license",
        "component_manifest",
        "bind_library",
        "version_history",
        "experimental_python_e2e_test",
    ):
        _assert_dicts_equal(a, b, [])
        return a

    if a["type"] == "cc_prebuilt_library":
        # This needs to go in each case to appease the type checker.
        assert a["type"] == b["type"]

        _assert_dicts_equal(a, b, ["binaries", "variants"])
        a["binaries"] = _merge_disjoint_dicts(
            a.get("binaries"), b.get("binaries")
        )
        a["variants"] = _merge_unique_variants(
            a.get("variants"),
            b.get("variants"),
            lambda v: v["constraints"],
        )
        return a

    if a["type"] == "loadable_module":
        assert a["type"] == b["type"]
        _assert_dicts_equal(a, b, ["binaries"])
        a["binaries"] = _merge_disjoint_dicts(
            a.get("binaries"), b.get("binaries")
        )
        return a

    if a["type"] == "package":
        assert a["type"] == b["type"]
        _assert_dicts_equal(a, b, ["variants"])
        a["variants"] = _merge_unique_variants(
            a.get("variants"),
            b.get("variants"),
            lambda v: (v["api_level"], v["arch"]),
        )
        return a

    if a["type"] == "sysroot":
        assert a["type"] == b["type"]
        _assert_dicts_equal(a, b, ["versions", "variants"])
        a["versions"] = _merge_disjoint_dicts(
            a.get("versions"), b.get("versions")
        )
        a["variants"] = _merge_unique_variants(
            a.get("variants"), b.get("variants"), lambda v: v["constraints"]
        )
        return a
    raise AssertionError("Unknown atom type: " + a["type"])
