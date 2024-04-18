#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from pathlib import Path
from typing import Callable, List, Set
from gn_label import GnLabel
import dataclasses
import os


@dataclasses.dataclass(frozen=False)
class FileAccess:
    """Manages access to the real file system, while keeping track of depfiles."""

    fuchsia_source_path: Path
    visited_files: Set[str] = dataclasses.field(default_factory=set)

    def read_text(self, label: GnLabel) -> str:
        """Reads the file into a text string"""
        GnLabel.check_type(label)
        path = self.fuchsia_source_path / label.path_str
        self.visited_files.add(path)
        return path.read_text()

    def file_exists(self, label: GnLabel) -> bool:
        """Whether the file exists and is not a directory"""
        GnLabel.check_type(label)
        path = self.fuchsia_source_path / label.path_str
        if path.exists() and path.is_file():
            self.visited_files.add(path)
            return True
        return False

    def directory_exists(self, label: GnLabel) -> bool:
        """Whether the directory exists and is indeed a directory"""
        GnLabel.check_type(label)
        path = self.fuchsia_source_path / label.path_str
        if path.exists() and path.is_dir():
            self.visited_files.add(path)
            return True
        return False

    def search_directory(
        self, label: GnLabel, path_predicate: Callable[[Path], bool]
    ) -> List[GnLabel]:
        """Lists the files in a directory corresponding with `label` (including files in subdirs) matching `path_predicate`"""
        GnLabel.check_type(label)
        path = self.fuchsia_source_path / label.path_str
        self.visited_files.add(path)

        output = []

        for root, _, files in os.walk(path):
            root_path = Path(root)
            for file in files:
                file_path = root_path / file
                if path_predicate(file_path):
                    relative_to_label = file_path.relative_to(
                        self.fuchsia_source_path
                    ).relative_to(label.path_str)
                    output.append(
                        label.create_child_from_str(str(relative_to_label))
                    )

        return output

    def write_depfile(self, dep_file_path: Path, main_entry: Path) -> None:
        if not dep_file_path.parent.exists():
            dep_file_path.parent.mkdir(parents=True, exist_ok=True)
        with open(dep_file_path, "w") as dep_file:
            dep_file.write(f"{main_entry}:\\\n")
            dep_file.write(
                "\\\n".join(sorted([f"    {p}" for p in self.visited_files]))
            )
