#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Collects licenses from GN metadata."""

import enum
from pathlib import Path
from gn_license_metadata import (
    GnApplicableLicensesMetadata,
    GnLicenseMetadataDB,
)
from typing import List, Set, Tuple
from gn_label import GnLabel
import dataclasses


@dataclasses.dataclass(frozen=True)
class CollectedLicense:
    """Data about a a single collected license (may have multiple files)."""

    public_name: str
    license_files: tuple[GnLabel]
    debug_hint: str = dataclasses.field(hash=False, compare=False)

    def create(
        public_name: str, license_files: Tuple[GnLabel], collection_hint: str
    ):
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


@dataclasses.dataclass
class Collector:
    """Collects licenses from build information"""

    fuchsia_source_path: Path
    metadata_db: GnLicenseMetadataDB
    unique_licenses: Set[CollectedLicense] = dataclasses.field(
        default_factory=set
    )
    errors: List[CollectorError] = dataclasses.field(default_factory=list)
    stats: CollectorStats = dataclasses.field(default_factory=CollectorStats)

    def _add_license(self, lic: CollectedLicense):
        if lic not in self.unique_licenses:
            self.unique_licenses.add(lic)
            self.stats.unique_licenses += 1

    def _add_license_from_label(
        self, license_label: GnLabel, used_by_target: GnLabel
    ):
        assert (
            license_label in self.metadata_db.licenses_by_label
        ), f"License target {license_label} not found: Referenced by {used_by_target}"
        license_metadata = self.metadata_db.licenses_by_label[license_label]
        self._add_license(
            CollectedLicense.create(
                public_name=license_metadata.public_package_name,
                license_files=license_metadata.license_files,
                collection_hint=f"From {license_metadata.target_label} used by {used_by_target}",
            )
        )

    def _add_error(self, error: CollectorError):
        self.errors.append(error)

    def _scan_target(self, target_metadata: GnApplicableLicensesMetadata):
        self.stats.targets_scanned += 1

        label = target_metadata.target_label
        license_labels = target_metadata.license_labels
        if license_labels:
            self.stats.targets_with_licenses += 1
            for license_label in license_labels:
                self._add_license_from_label(
                    license_label=license_label,
                    used_by_target=label,
                )
        elif label.is_3rd_party():
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
