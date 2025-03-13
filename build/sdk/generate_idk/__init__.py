#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functionality for building the IDK from its component parts."""

# See https://stackoverflow.com/questions/33533148/how-do-i-type-hint-a-method-with-the-type-of-the-enclosing-class
from __future__ import annotations

import dataclasses
import filecmp
import itertools
import json
import pathlib
from typing import (
    Any,
    Callable,
    Literal,
    Mapping,
    Optional,
    Sequence,
    TypedDict,
    TypeVar,
)

# version_history.json doesn't follow the same schema as other IDK metadata
# files, so we treat it specially in a few places.
VERSION_HISTORY_PATH = pathlib.Path("version_history.json")


STABLE_SUPPORTED_TYPES = ["cc_source_library", "fidl_library"]


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
    stable: bool


class SysrootMeta(TypedDict):
    name: str
    type: Literal["sysroot"]
    versions: dict[str, Any]
    variants: list[Any]
    stable: bool


class PackageMeta(TypedDict):
    name: str
    type: Literal["package"]
    variants: list[Any]
    stable: bool


class LoadableModuleMeta(TypedDict):
    name: str
    type: Literal["loadable_module"]
    binaries: dict[str, Any]
    stable: bool


class UnmergableMeta(TypedDict):
    name: str
    type: (
        Literal["cc_source_library"]
        | Literal["dart_library"]
        | Literal["fidl_library"]
        | Literal["documentation"]
        | Literal["config"]
        | Literal["license"]
        | Literal["component_manifest"]
        | Literal["bind_library"]
        | Literal["version_history"]
        | Literal["experimental_python_e2e_test"]
    )
    stable: bool


AtomMeta = (
    CCPrebuiltLibraryMeta
    | LoadableModuleMeta
    | PackageMeta
    | SysrootMeta
    | UnmergableMeta
)


@dataclasses.dataclass
class PartialAtom:
    """Metadata and files associated with a single Atom from a single subbuild.

    Attributes:
        meta (AtomMeta): JSON object from the atom's `meta.json` file.
        meta_src (pathlib.Path): The path from which `meta` was read.
        dest_to_src (dict[pathlib.Path, pathlib.Path]): All non-metadata files
            associated with this atom belong in this dictionary. The key is the
            file path relative to the final IDK directory. The value is either
            absolute or relative to the current working directory.
    """

    meta: AtomMeta
    meta_src: pathlib.Path
    dest_to_src: dict[pathlib.Path, pathlib.Path]


@dataclasses.dataclass
class PartialIDK:
    """A model of the parts of an IDK from a single subbuild.

    Attributes:
        manifest_src (pathlib.Path): Source path for the overall build manifest.
            Either absolute or relative to the current working directory.
        atoms (dict[pathlib.Path, PartialAtom]): Atoms to include in the IDK,
            indexed by the path to their metadata file, relative to the final
            IDK directory (e.g., `bind/fuchsia.ethernet/meta.json`).
    """

    manifest_src: pathlib.Path
    atoms: dict[pathlib.Path, PartialAtom]

    @staticmethod
    def load(
        build_dir: pathlib.Path, relative_manifest_path: pathlib.Path
    ) -> PartialIDK:
        """Load relevant information about a piece of the IDK from a subbuild
        dir."""
        result = PartialIDK(
            manifest_src=(build_dir / relative_manifest_path), atoms={}
        )
        with (build_dir / relative_manifest_path).open() as f:
            build_manifest: BuildManifestJson = json.load(f)

        for atom in build_manifest["atoms"]:
            # sdk_noop_atoms have no metadata specified. Skip them.
            if not atom["meta"]:
                continue

            meta_dest = pathlib.Path(atom["meta"])
            meta_src = None
            dest_to_src = {}
            for file in atom["files"]:
                src_path = build_dir / file["source"]
                dest_path = pathlib.Path(file["destination"])

                # Determine if this file is the metadata file for this atom.
                if dest_path == meta_dest:
                    meta_src = src_path
                else:
                    assert dest_path not in dest_to_src, (
                        "File specified multiple times in atom: %s" % dest_path
                    )
                    dest_to_src[dest_path] = src_path

            assert meta_src, (
                "Atom does not include its metadata file in 'files': %s" % atom
            )

            with meta_src.open() as f:
                assert meta_dest not in result.atoms, (
                    "Atom metadata file specified multiple times: %s"
                    % meta_dest
                )
                result.atoms[meta_dest] = PartialAtom(
                    meta=json.load(f),
                    meta_src=meta_src,
                    dest_to_src=dest_to_src,
                )

        return result

    def input_files(self) -> set[pathlib.Path]:
        """Return the set of input files in this PartialIDK for generating a
        depfile."""
        result = set()
        result.add(self.manifest_src)
        for atom in self.atoms.values():
            result.add(atom.meta_src)
            result |= set(atom.dest_to_src.values())
        return result


class AtomMergeError(Exception):
    def __init__(self, atom_path: pathlib.Path):
        super(AtomMergeError, self).__init__(
            "While merging atom: %s" % atom_path
        )


@dataclasses.dataclass
class MergedIDK:
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

    atoms: dict[pathlib.Path, AtomMeta] = dataclasses.field(
        default_factory=dict
    )
    dest_to_src: dict[pathlib.Path, pathlib.Path] = dataclasses.field(
        default_factory=dict
    )

    def merge_with(self, other: PartialIDK) -> MergedIDK:
        """Merge the contents of this MergedIDK with a PartialIDK and return the
        result.

        Put enough of them together, and you get a full IDK!
        """
        result = MergedIDK(
            atoms=_merge_atoms(self.atoms, other.atoms),
            dest_to_src=self.dest_to_src,
        )

        for atom in other.atoms.values():
            result.dest_to_src = _merge_other_files(
                result.dest_to_src, atom.dest_to_src
            )
        return result

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

            if type in STABLE_SUPPORTED_TYPES:
                index.append(
                    dict(
                        meta=str(meta_path),
                        type=type,
                        stable=atom.get("stable", False),
                    )
                )
            else:
                index.append(dict(meta=str(meta_path), type=type, stable=True))

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
    a: dict[pathlib.Path, AtomMeta], b: dict[pathlib.Path, PartialAtom]
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
                    atom_a == atom_b.meta
                ), "A and B had different 'version_history' values. Huh?"
                result[atom_path] = atom_a
            else:
                # Merge atoms found in both IDKs.
                try:
                    result[atom_path] = _merge_atom_meta(atom_a, atom_b.meta)
                except Exception as e:
                    raise AtomMergeError(atom_path) from e
        elif atom_a:
            result[atom_path] = atom_a
        else:
            assert atom_b, "unreachable. Atom '%s' had falsy value?" % atom_path
            result[atom_path] = atom_b.meta
    return result


def _merge_other_files(
    a: dict[pathlib.Path, pathlib.Path],
    b: dict[pathlib.Path, pathlib.Path],
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


def _merge_atom_meta(a: AtomMeta, b: AtomMeta) -> AtomMeta:
    """Merge two atoms, according to type-specific rules."""
    if a["type"] in (
        "cc_source_library",
        "dart_library",
        "fidl_library",
        "documentation",
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

        # "binaries" contains the legacy API level unaware prebuilts, and
        # "variants" contains the API level-specific prebuilts. They are
        # mutually exclusive for a given [sub-]build.
        # The root "ifs" file corresponds to "binaries" and thus does not
        # exist in atoms with "variants".
        _assert_dicts_equal(a, b, ["binaries", "variants", "ifs"])
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
