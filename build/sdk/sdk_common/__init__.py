# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import collections
import difflib
import functools
import json
import pathlib
import yaml

from typing import Sequence, Optional, List, Any, Iterator


class File:
    """Wrapper class for file definitions."""

    def __init__(self, json) -> None:
        self.source: str = json["source"]
        self.destination: str = json["destination"]

    def __str__(self) -> str:
        return "{%s <-- %s}" % (self.destination, self.source)


@functools.total_ordering
class Atom(object):
    """Wrapper class for atom data, adding convenience methods."""

    def __init__(self, json) -> None:
        self.json = json
        self.id: str = json["id"]
        self.metadata: str = json["meta"]
        self.label: str = json["gn-label"]
        self.category: str = json["category"]
        self.area: Optional[str] = json["area"]
        self.deps: Sequence[str] = json["deps"]
        self.files: Sequence[File] = [File(f) for f in json["files"]]
        self.type: str = json["type"]

    def __str__(self):
        return str(self.id)

    def __hash__(self):
        return hash(self.label)

    def __eq__(self, other):
        return self.label == other.label

    def __ne__(self, other):
        return not __eq__(self, other)

    def __lt__(self, other):
        return self.id < other.id


def gather_dependencies(manifests):
    """Extracts the set of all required atoms from the given manifests, as well
    as the set of names of all the direct dependencies.
    """
    direct_deps = set()
    atoms = set()

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
    "excluded",
    "experimental",
    "internal",
    "cts",
    "partner_internal",
    "partner",
    "public",
]


def _index_for_category(category: str):
    if not category in CATEGORIES:
        raise Exception('Unknown SDK category "%s"' % category)
    return CATEGORIES.index(category)


def detect_category_violations(
    category: str, atoms: Sequence[Atom]
) -> Iterator[str]:
    """Yields strings describing mismatches in publication categories."""
    category_index = _index_for_category(category)
    for atom in atoms:
        if _index_for_category(atom.category) < category_index:
            yield (
                "%s has publication level %s, incompatible with %s"
                % (atom, atom.category, category)
            )


def area_names_from_file(parsed_areas: Any) -> List[str]:
    """Given a parsed version of docs/contribute/governance/areas/_areas.yaml,
    return a list of acceptable area names."""
    return [area["name"] for area in parsed_areas] + ["Unknown"]


_AREA_REQUIRED_CATEGORIES = ["public", "partner", "partner_internal"]
_AREA_OPTIONAL_TYPES = [
    "cc_prebuilt_library",
    "cc_source_library",
    "companion_host_tool",
    "dart_library",
    "data",
    "documentation",
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
        with areas_file.open() as f:
            parsed_areas = yaml.safe_load(f)
            return Validator(area_names_from_file(parsed_areas))

    def detect_violations(
        self, category: Optional[str], atoms: Sequence[Atom]
    ) -> Iterator[str]:
        """Yield strings describing all violations found in `atoms`."""
        yield from detect_collisions(atoms)
        if category:
            yield from detect_category_violations(category, atoms)
        yield from self.detect_area_violations(atoms)

    def detect_area_violations(self, atoms: Sequence[Atom]) -> Iterator[str]:
        """Yields strings describing any invalid API areas in `atoms`."""
        for atom in atoms:
            if (
                atom.area is None
                and atom.category in _AREA_REQUIRED_CATEGORIES
                and atom.type not in _AREA_OPTIONAL_TYPES
            ):
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
