# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import collections
import difflib
import functools
import json
import pathlib
from typing import Any, Iterator, Sequence


class File:
    """Wrapper class for file definitions."""

    def __init__(self, json: dict[str, Any]) -> None:
        self.source: str = json["source"]
        self.destination: str = json["destination"]

    def __str__(self) -> str:
        return "{%s <-- %s}" % (self.destination, self.source)


@functools.total_ordering
class Atom(object):
    """Wrapper class for atom data, adding convenience methods."""

    def __init__(self, json: dict[str, Any]) -> None:
        self.json = json
        self.id: str = json["id"]
        self.metadata: str = json["meta"]
        self.label: str = json["gn-label"]
        self.category: str = json["category"]
        self.area: str | None = json["area"]
        self.deps: Sequence[str] = json["deps"]
        self.files: Sequence[File] = [File(f) for f in json["files"]]
        self.type: str = json["type"]

    def __str__(self) -> str:
        return str(self.id)

    def __hash__(self) -> int:
        return hash(self.label)

    def __eq__(self, other: Any) -> bool:
        if not isinstance(other, Atom):
            return False
        return self.label == other.label

    def __ne__(self, other: Any) -> bool:
        if not isinstance(other, Atom):
            return True
        return not self.__eq__(other)

    def __lt__(self, other: Any) -> bool:
        if not isinstance(other, Atom):
            return False
        return self.id < other.id


def gather_dependencies(
    manifests: list[str] | None,
) -> tuple[set[str], set[Atom]]:
    """Extracts the set of all required atoms from the given manifests, as well
    as the set of names of all the direct dependencies.
    """
    direct_deps: set[str] = set()
    atoms: set[Atom] = set()

    if manifests is None:
        return (direct_deps, atoms)

    for dep in manifests:
        with open(dep, "r") as dep_file:
            dep_manifest = json.load(dep_file)
            direct_deps.update(dep_manifest["ids"])
            atoms.update([Atom(a) for a in dep_manifest["atoms"]])
    return (direct_deps, atoms)


def detect_collisions(atoms: Sequence[Atom]) -> Iterator[str]:
    """Detects name collisions in a given atom list. Yields a series of error
    messages as strings."""
    mappings = collections.defaultdict(lambda: [])
    for atom in atoms:
        mappings[atom.id].append(atom)
    for id, group in mappings.items():
        if len(group) == 1:
            continue
        labels = [a.label for a in group]
        msg = "Targets sharing the SDK id %s:\n" % id
        for label in labels:
            msg += " - %s\n" % label
        yield msg


CATEGORIES = [
    # "excluded" is deprecated.
    # "experimental" is deprecated.
    "internal",
    "cts",
    "partner_internal",
    "partner",
    "public",
]


def _index_for_category(category: str) -> int:
    if not category in CATEGORIES:
        raise Exception('Unknown SDK category "%s"' % category)
    return CATEGORIES.index(category)


def detect_category_violations(
    category: str, atoms: Sequence[Atom]
) -> Iterator[str]:
    """Yields strings describing mismatches in publication categories."""
    category_index = _index_for_category(category)
    for atom in atoms:
        # "cts" is not properly implemented and should not be used. See
        # https://fxbug.dev/367760026.
        # The public IDK does not yet exist, so "public" should not be used.
        if atom.category == "cts" or atom.category == "public":
            raise Exception(
                '"%s" has SDK category "%s", which is not yet supported.'
                % (atom, atom.category)
            )
        if _index_for_category(atom.category) < category_index:
            yield (
                '"%s" has publication level "%s", which is incompatible with "%s".'
                % (atom, atom.category, category)
            )


def area_names_from_file(parsed_areas: Any) -> list[str]:
    """Given a parsed version of docs/contribute/governance/areas/_areas.yaml,
    return a list of acceptable area names."""
    return [area["name"] for area in parsed_areas] + ["Unknown"]


_AREA_OPTIONAL_TYPES = [
    "cc_prebuilt_library",
    "cc_source_library",
    "companion_host_tool",
    "dart_library",
    "data",
    "documentation",
    "experimental_python_e2e_test",
    "ffx_tool",
    "host_tool",
    "loadable_module",
    "package",
    "sysroot",
    "version_history",
]


class Validator:
    """Helper class to validate sets of IDK atoms."""

    def __init__(self, valid_areas: Sequence[str]) -> None:
        """Construct a validator with a given set of areas. Exposed for
        testing. Use Validator.from_areas_file_path instead."""
        self._valid_areas = valid_areas

    @classmethod
    def from_areas_file_path(cls, areas_file: pathlib.Path) -> "Validator":
        """Build a Validator given a path to
        docs/contribute/governance/areas/_areas.yaml."""
        import yaml

        with areas_file.open() as f:
            parsed_areas = yaml.safe_load(f)
            return Validator(area_names_from_file(parsed_areas))

    def detect_violations(
        self, category: str | None, atoms: Sequence[Atom]
    ) -> Iterator[str]:
        """Yield strings describing all violations found in `atoms`."""
        yield from detect_collisions(atoms)
        if category:
            yield from detect_category_violations(category, atoms)
        yield from self.detect_area_violations(atoms)

    def detect_area_violations(self, atoms: Sequence[Atom]) -> Iterator[str]:
        """Yields strings describing any invalid API areas in `atoms`."""
        for atom in atoms:
            if atom.area is None and atom.type not in _AREA_OPTIONAL_TYPES:
                yield (
                    "%s must specify an API area. Valid areas: %s"
                    % (
                        atom,
                        self._valid_areas,
                    )
                )

            if atom.area is not None and atom.area not in self._valid_areas:
                if matches := difflib.get_close_matches(
                    atom.area, self._valid_areas
                ):
                    yield (
                        "%s specifies invalid API area '%s'. Did you mean one of these? %s"
                        % (atom, atom.area, matches)
                    )
                else:
                    yield (
                        "%s specifies invalid API area '%s'. Valid areas: %s"
                        % (atom, atom.area, self._valid_areas)
                    )
