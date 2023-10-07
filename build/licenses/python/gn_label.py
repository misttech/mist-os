#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities for working with GnLabel strings."""

import dataclasses
from pathlib import Path
from typing import List


@dataclasses.dataclass(frozen=True)
class GnLabel:
    """Utility for handling Gn target labels"""

    """The original GN label string, e.g. `//foo/bar:baz(//toolchain)`"""
    gn_str: str
    """The path part of the label, e.g. `foo/bar` in `//foo/bar:baz`"""
    path: Path = dataclasses.field(hash=False, compare=False)
    """The name part of the label, e.g. `baz` in `//foo/bar:baz` or `bar` for `//foo/bar`"""
    name: str = dataclasses.field(hash=False, compare=False)
    """Whether the label has a local name, e.g. True for `//foo/bar:baz` but false for `//foo/bar/baz`"""
    is_local_name: bool = dataclasses.field(hash=False, compare=False)
    """The toolchain part of the label, e.g. `//toolchain` in `//foo/bar(//toolchain)`"""
    toolchain: "GnLabel" = dataclasses.field(hash=False, compare=False)

    def from_str(original_str: str) -> "GnLabel":
        """Constructs a GnLabel instance from a GN target label string"""
        assert original_str.startswith(
            "//"
        ), f"label must start with // but got {original_str}"
        assert (
            ".." not in original_str
        ), f".. is not supported but got {original_str}"
        label_and_toolchain = original_str.split("(", maxsplit=1)
        if len(label_and_toolchain) == 2:
            toolchain = GnLabel.from_str(label_and_toolchain[1][:-1])
        else:
            assert len(label_and_toolchain) == 1
            toolchain = None
        label = label_and_toolchain[0]
        path_and_name = label.split(":", maxsplit=1)
        path = Path(path_and_name[0][2:])  # remove '//'
        if len(path_and_name) == 2:
            name = path_and_name[1]
            is_local_name = True
        else:
            assert len(path_and_name) == 1
            name = str(path.name)
            is_local_name = False

        return GnLabel(
            gn_str=original_str,
            name=name,
            is_local_name=is_local_name,
            path=path,
            toolchain=toolchain,
        )

    def from_path(path: Path):
        """Constructs a GnLabel instance from a Path object."""
        assert isinstance(
            path, Path
        ), f"Expected path of type Path but got {type(path)}"
        if path.name == "" and path.parent.name == "":
            return GnLabel.from_str("//")
        return GnLabel.from_str(f"//{path}")

    def check_type(other) -> "GnLabel":
        """Asserts that `other` is of type GnLabel"""
        assert isinstance(
            other, GnLabel
        ), f"{other} type {type(other)} is not {GnLabel}"
        return other

    def check_types_in_list(list: List["GnLabel"]) -> List["GnLabel"]:
        """Asserts that all values in `list` are of type GnLabel"""
        for v in list:
            GnLabel.check_type(v)
        return list

    def parent_label(self) -> "GnLabel":
        """Returns //foo/bar for //foo/bar/baz and //foo/bar:baz"""
        assert self.has_parent_label(), f"{self} has no parent label"
        if self.name == self.path.name:
            return GnLabel.from_path(self.path.parent)
        else:
            return GnLabel.from_path(self.path)

    def has_parent_label(self) -> bool:
        return self.gn_str != "//"

    def without_toolchain(self) -> "GnLabel":
        """Removes the toolchain part of the label"""
        if self.toolchain:
            return GnLabel.from_str(self.gn_str.split("(", maxsplit=1)[0])
        else:
            return self

    def ensure_toolchain(self, toolchain: "GnLabel") -> "GnLabel":
        """Sets a toolchain if there is none already"""
        if self.toolchain:
            return self
        else:
            return dataclasses.replace(
                self, toolchain=toolchain, gn_str=f"{self.gn_str}({toolchain})"
            )

    def rebased_path(self, base_dir: Path) -> Path:
        """Returns package_path rebased to a given base_dir."""
        return base_dir / self.path

    def code_search_url(self) -> str:
        """Returns package_path rebased to a given base_dir."""
        path = self.path
        return f"https://cs.opensource.google/fuchsia/fuchsia/+/main:{path}"

    def is_host_target(self) -> bool:
        return self.toolchain and self.toolchain.name.startswith("host_")

    def is_3rd_party(self) -> bool:
        return "third_party" in self.gn_str

    def is_3p_rust_crate(self) -> bool:
        return self.gn_str.startswith("//third_party/rust_crates:")

    def is_3p_golib(self) -> bool:
        return self.gn_str.startswith("//third_party/golibs:")

    def create_child(self, child_path: Path) -> "GnLabel":
        """Create a child label relative to this label"""
        assert isinstance(child_path, Path)
        return GnLabel.from_path(self.path / child_path)

    def create_child_from_str(self, child_path_str: str) -> "GnLabel":
        """Create a GnLabel relative to this label from a child path GN string"""
        if child_path_str.startswith("//"):
            return GnLabel.from_str(child_path_str)
        elif child_path_str.startswith(":"):
            assert (
                not self.is_local_name
            ), f"Can't apply {child_path_str} to {self}"
            return GnLabel.from_str(f"//{self.path}:{child_path_str[1:]}")
        elif child_path_str.startswith("../"):
            assert (
                self.has_parent_label()
            ), f"Can't apply {child_path_str} to {self}"
            parent_label = self.parent_label()
            if self.is_local_name:
                parent_label = parent_label.parent_label()
            return parent_label.create_child_from_str(child_path_str[3:])
        else:
            return GnLabel.from_path(self.path / Path(child_path_str))

    def __str__(self) -> str:
        return self.gn_str

    def __repr__(self) -> str:
        return f"{self.__class__.__name__}({self.gn_str})"

    def __gt__(self, other: "GnLabel") -> bool:
        return self.gn_str > other.gn_str
