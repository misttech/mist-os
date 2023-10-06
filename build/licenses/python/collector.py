#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Collects licenses from GN metadata."""

from collections import defaultdict
import enum
import logging
from pathlib import Path
from readme_fuchsia import ReadmesDB, Readme
from gn_license_metadata import (
    GnApplicableLicensesMetadata,
    GnLicenseMetadataDB,
)
from typing import Dict, List, Set, Tuple
from gn_label import GnLabel
from file_access import FileAccess
import dataclasses


@dataclasses.dataclass(frozen=True)
class CollectedLicense:
    """Data about a a single collected license (may have multiple files)."""

    public_name: str
    license_files: tuple[GnLabel]
    debug_hint: str = dataclasses.field(hash=False, compare=False)

    def create(
        public_name: str, license_files: Tuple[GnLabel], collection_hint: str
    ) -> "CollectedLicense":
        assert type(public_name) is str
        assert type(license_files) is tuple
        GnLabel.check_types_in_list(license_files)
        assert type(collection_hint) is str
        return CollectedLicense(
            public_name, tuple(sorted(license_files)), collection_hint
        )

    def __str__(self) -> str:
        return (
            """License(name='{name}', files=[{files}], hint='{hint}')""".format(
                name=self.name,
                files=", ".join([str(f) for f in self.license_files]),
                hint=self.debug_hint,
            )
        )


class CollectorErrorKind(enum.Enum):
    """The type of a CollectionError"""

    THIRD_PARTY_TARGET_WITHOUT_APPLICABLE_LICENSES = 1

    LICENSE_FILE_IN_README_NOT_FOUND = 2

    NO_LICENSE_FILE_IN_README = 3

    # TODO(133985): More error kinds to come


@dataclasses.dataclass(frozen=True)
class CollectorError:
    """Describe collection error encountered by Collector"""

    kind: CollectorErrorKind
    target_label: GnLabel
    debug_hint: str = dataclasses.field(hash=False, compare=False)


@dataclasses.dataclass(frozen=False)
class CollectorStats:
    """Collector execution stats"""

    targets_loaded: int = 0
    targets_scanned: int = 0
    targets_with_licenses: int = 0
    unique_licenses: int = 0
    licenses_from_readmes: int = 0
    licenses_from_metadata: int = 0


@dataclasses.dataclass
class Collector:
    """Collects licenses from build information"""

    file_access: FileAccess
    metadata_db: GnLicenseMetadataDB
    readmes_db: ReadmesDB
    unique_licenses: Set[CollectedLicense] = dataclasses.field(
        default_factory=set
    )
    errors: List[CollectorError] = dataclasses.field(default_factory=list)
    stats: CollectorStats = dataclasses.field(default_factory=CollectorStats)

    def _add_license(self, lic: CollectedLicense):
        if lic not in self.unique_licenses:
            self.unique_licenses.add(lic)
            self.stats.unique_licenses += 1

    def _add_license_from_metadata(
        self, license_metadata_label: GnLabel, used_by_target: GnLabel
    ):
        assert (
            license_metadata_label in self.metadata_db.licenses_by_label
        ), f"License target {license_metadata_label} not found: Referenced by {used_by_target}"
        license_metadata = self.metadata_db.licenses_by_label[
            license_metadata_label
        ]
        self._add_license(
            CollectedLicense.create(
                public_name=license_metadata.public_package_name,
                license_files=license_metadata.license_files,
                collection_hint=f"From {license_metadata.target_label} used by {used_by_target}",
            )
        )

    def _add_license_from_readme(self, readme: Readme, used_by_target: GnLabel):
        # Ensure readme has files:
        if not readme.license_files:
            self._add_error(
                CollectorError(
                    kind=CollectorErrorKind.NO_LICENSE_FILE_IN_README,
                    target_label=used_by_target,
                    debug_hint=f"In {readme.readme_label}",
                )
            )
            return

        # Ensure files actually exist:
        for lic_file in readme.license_files:
            if not self.file_access.file_exists(lic_file):
                self._add_error(
                    CollectorError(
                        kind=CollectorErrorKind.LICENSE_FILE_IN_README_NOT_FOUND,
                        target_label=used_by_target,
                        debug_hint=f"In {lic_file} specified in {readme.readme_label}",
                    )
                )
                return

        license = CollectedLicense.create(
            public_name=readme.package_name,
            license_files=tuple(readme.license_files),
            collection_hint=f"for {used_by_target} via {readme.readme_label}",
        )

        self.stats.licenses_from_readmes += 1
        self._add_license(license)

    def _add_error(self, error: CollectorError):
        self.errors.append(error)

    def _scan_target(self, target_metadata: GnApplicableLicensesMetadata):
        self.stats.targets_scanned += 1

        license_found = True

        label = target_metadata.target_label
        license_labels = target_metadata.license_labels
        if license_labels:
            self.stats.targets_with_licenses += 1
            license_found = True
            for license_label in license_labels:
                self.stats.licenses_from_metadata += 1
                self._add_license_from_metadata(
                    license_metadata_label=license_label,
                    used_by_target=label,
                )

        if not license_found:
            readme = self.readmes_db.find_readme_for_label(label)
            if readme and readme.license_files:
                self._add_license_from_readme(readme, used_by_target=label)
                license_found = True

        if target_metadata.target_type == "group":
            # TODO(133985): Handle prebuilt and direct source dependencies.
            return

        if (
            not license_found
            and label.is_3rd_party()
            and target_metadata != "group"
        ):
            # 3P Targets require applicable licenses.
            self._add_error(
                CollectorError(
                    kind=CollectorErrorKind.THIRD_PARTY_TARGET_WITHOUT_APPLICABLE_LICENSES,
                    target_label=label,
                    debug_hint=None,
                )
            )

        # TODO(133985): Handle prebuilt and direct source dependencies.

    def collect(self):
        for (
            target_metadata
        ) in self.metadata_db.applicable_licenses_by_target.values():
            self._scan_target(target_metadata)

    def log_errors(self, log_level: int = logging.ERROR):
        errors_by_kind: Dict[
            CollectorErrorKind, List[CollectorError]
        ] = defaultdict(list)

        for error in self.errors:
            errors_by_kind[error.kind].append(error)

        message_lines = [
            f"Encountered {len(self.errors)} license collection errors."
        ]

        for kind in errors_by_kind.keys():
            sub_errors = errors_by_kind[kind]
            count = len(sub_errors)
            message_lines.append(f"There are {count} {kind.name} errors for:")

            sub_message_lines = []
            for e in sub_errors:
                sub_message_lines.append(
                    f"  {e.target_label}  hint={e.debug_hint}"
                )
            sub_message_lines.sort()

            max_lines = 500
            if max_lines < len(sub_message_lines):
                trim_message = (
                    f"  (and {len(sub_message_lines) - max_lines} more)"
                )
                sub_message_lines = sub_message_lines[0:max_lines]
                sub_message_lines.append(trim_message)

            message_lines.extend(sub_message_lines)

        logging.log(log_level, "\n".join(message_lines))
