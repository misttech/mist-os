#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import dataclasses
import json
import logging
from pathlib import Path
import re
from typing import Any, Dict, List, Set
import hashlib
from gn_label import GnLabel


@dataclasses.dataclass(frozen=True)
class _ExtractedLicense:
    """Per-license information that is used for comparisons"""

    name: str
    text_hash: str

    # Debugging fields: Not part of compare or hash
    spdx_id: str = dataclasses.field(compare=False, hash=False, default=None)
    text_sample: str = dataclasses.field(
        compare=False, hash=False, default=None
    )
    debug_hint: str = dataclasses.field(compare=False, hash=False, default=None)

    def from_json_dict(input: Dict[str, Any]) -> "_ExtractedLicense":
        # Example SPDX license dict:
        # {
        #     "name": "Bar Pkg",
        #     "licenseId": "LicenseRef-1234",
        #     "extractedText": "some text",
        #     "crossRefs": [
        #         {
        #             "url": "https://cs.opensource.google/fuchsia/fuchsia/+/main:bar/license"
        #         }
        #     ]
        # },

        name: str = input["name"].lower()

        # Remove version suffix which is common in rust crates in the legacy SPDX
        version_suffix_pattern = re.compile("-[\d.]+")
        version_suffix_match = version_suffix_pattern.search(name)
        if version_suffix_match:
            name = name[0 : version_suffix_match.start()]
        name = name.replace("-", "_")

        spdx_id: str = input["licenseId"]
        text: str = input["extractedText"]

        text = text.replace(
            "\r", ""
        )  # Remove carriage returns. check-license has them, but python pipeline doesn't.

        sha = hashlib.sha256()
        sha.update(text.encode())
        text_hash = sha.hexdigest()[0:8]
        text_sample = text
        max_sample_size = 32
        if len(text_sample) > max_sample_size:
            text_sample = text_sample[0 : max_sample_size - 3] + "..."

        # Escape all kinds of newlines
        text_sample = text_sample.encode("unicode_escape").decode("utf-8")

        debug_hint = None
        if "crossRefs" in input:
            debug_hint = input["crossRefs"][0]["url"]
        elif "seeAlsos" in input:
            debug_hint = input["seeAlsos"][0]
        if debug_hint:
            debug_hint = debug_hint.replace(
                "https://cs.opensource.google/fuchsia/fuchsia/+/main:", ""
            )
            debug_hint = debug_hint.replace(
                "https://fuchsia.googlesource.com/fuchsia/+/${GIT_REVISION}/",
                "",
            )

        return _ExtractedLicense(
            name=name,
            spdx_id=spdx_id,
            text_hash=text_hash,
            text_sample=text_sample,
            debug_hint=debug_hint,
        )

    def __gt__(self, other) -> bool:
        return self.name > other.name


# TODO(132725): Remove once migration completes.
@dataclasses.dataclass(frozen=True)
class SpdxComparator:
    """Utility for comparing the licenses in 2 spdx files"""

    current_file: Path
    legacy_file: Path

    all: Set[_ExtractedLicense] = dataclasses.field(default_factory=set)
    in_both: Set[_ExtractedLicense] = dataclasses.field(default_factory=set)
    added: Set[_ExtractedLicense] = dataclasses.field(default_factory=set)
    missing: Set[_ExtractedLicense] = dataclasses.field(default_factory=set)

    def compare(self):
        """Returns whether the files have the same licenses"""
        current_lics = self._read_spdx_licenses(self.current_file)
        legacy_lics = self._read_spdx_licenses(self.legacy_file)

        self.all.update(current_lics)
        self.all.update(legacy_lics)
        self.in_both.update(current_lics.intersection(legacy_lics))
        self.added.update(current_lics.difference(legacy_lics))
        self.missing.update(legacy_lics.difference(current_lics))

        # Some consistency checks
        assert not self.added.intersection(self.missing)
        assert len(self.all) == len(self.in_both) + len(self.added) + len(
            self.missing
        )

    def _read_spdx_licenses(self, path) -> Set[_ExtractedLicense]:
        with open(path, "r") as spdx_file:
            spdx_doc = json.load(spdx_file)
            output = set()
            if "hasExtractedLicensingInfos" in spdx_doc:
                for d in spdx_doc["hasExtractedLicensingInfos"]:
                    output.add(_ExtractedLicense.from_json_dict(d))
            return output

    def found_differences(self) -> bool:
        return self.added or self.missing

    def log_differences(self, log_level: int):
        message_lines = []
        if self.added or self.missing:
            message_lines.append(
                f"{self.current_file} has DIFFERENT licenses then {self.legacy_file}:"
            )

            for el in sorted(list(self.all)):
                if el in self.in_both:
                    prefix = " "
                elif el in self.added:
                    prefix = "+"
                elif el in self.missing:
                    prefix = "-"
                else:
                    assert False, "unreachable"
                message_lines.append(
                    f'{prefix} {el.name}         {el.text_hash} {el.debug_hint} sample="{el.text_sample}" spdx_id="{el.spdx_id}"'
                )

            message_lines.append(f"Same: {len(self.in_both)}")
            if self.added:
                message_lines.append(f"Added: {len(self.added)}")
            if self.missing:
                message_lines.append(f"Missing: {len(self.missing)}")
        else:
            message_lines.append(
                f"{self.current_file} has the SAME {len(self.in_both)} licenses then {self.legacy_file}."
            )

        logging.log(log_level, "\n".join(message_lines))
