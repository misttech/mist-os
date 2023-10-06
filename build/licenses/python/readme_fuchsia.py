#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import dataclasses
from gn_label import GnLabel
from file_access import FileAccess
from typing import Dict, List, Tuple, Any, Set


@dataclasses.dataclass
class Readme:
    readme_label: GnLabel
    package_name: str
    license_files: Tuple[GnLabel]

    def from_text(
        readme_label: GnLabel, applicable_target: GnLabel, file_text: str
    ) -> "Readme":
        package_name = None
        license_files: List[GnLabel] = []
        lines = file_text.split("\n")
        for l in lines:
            if ":" in l:
                splitted_line = [s.strip() for s in l.split(":")]
                key = splitted_line[0].upper()
                value = splitted_line[1]
                if key == "NAME":
                    package_name = value
                elif key == "LICENSE FILE":
                    license_file = applicable_target.create_child_from_str(
                        value
                    )
                    license_files.append(license_file)

        if not package_name:
            raise RuntimeError(
                f"File {readme_label} does not contain `Name: [the public name of the package]"
            )

        return Readme(
            readme_label=readme_label,
            package_name=package_name,
            license_files=tuple(license_files),
        )


@dataclasses.dataclass
class ReadmesDB:
    file_access: FileAccess
    cache: Dict[GnLabel, Readme] = dataclasses.field(default_factory=dict)

    def find_readme_for_label(self, target_label: GnLabel) -> Readme:
        GnLabel.check_type(target_label)

        if target_label.toolchain:
            target_label = target_label.without_toolchain()
        if target_label.is_local_name:
            target_label = target_label.parent_label()

        if target_label in self.cache:
            return self.cache[target_label]

        potential_readme_files = [
            "vendor/google/tools/check-licenses/assets/readmes"
            / target_label.path
            / "README.fuchsia",
            "tools/check-licenses/assets/readmes"
            / target_label.path
            / "README.fuchsia",
            target_label.path / "README.fuchsia",
        ]
        for readme_source_path in potential_readme_files:
            # TODO(133985): Handle barrier dirs

            readme_label = GnLabel.from_path(readme_source_path)
            if self.file_access.file_exists(readme_label):
                readme = Readme.from_text(
                    readme_label,
                    applicable_target=target_label,
                    file_text=self.file_access.read_text(readme_label),
                )
                self.cache[target_label] = readme
                return readme

        if target_label.has_parent_label():
            self.cache[target_label] = self.find_readme_for_label(
                target_label.parent_label()
            )
        else:
            self.cache[target_label] = None
        return self.cache[target_label]
