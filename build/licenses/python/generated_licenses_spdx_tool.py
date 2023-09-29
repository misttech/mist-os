#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates licenses SPDX from GN metadata."""

import argparse
import sys
import logging
from spdx_writer import SpdxWriter
from collector import Collector
from pathlib import Path
from gn_license_metadata import GnLicenseMetadataDB


def main():
    """
    Generates licenses SPDX json file from GN license metadata.
    """
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--generated-license-metadata",
        type=str,
        required=True,
        help="Path to the license GN metadata file, generated via the GN genereated_file action",
    )
    parser.add_argument(
        "--fuchsia-source-path",
        type=str,
        required=True,
        help="The path to the root of the Fuchsia source tree",
    )
    parser.add_argument(
        "--spdx-root-package-name",
        type=str,
        required=True,
        help="The name of the SPDX root package",
    )
    parser.add_argument(
        "--spdx-output",
        type=str,
        required=True,
        help="Path to the output SPDX file",
    )
    parser.add_argument(
        "--dep-file",
        type=str,
        required=True,
        help="Path to the generated dep file",
    )

    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO, force="format='%(levelname)s:%(message)s'"
    )

    fuchsia_source_path = Path(args.fuchsia_source_path).expanduser()
    assert fuchsia_source_path.exists()

    # Collect licenses information
    collector = Collector(
        fuchsia_source_path=fuchsia_source_path,
        metadata_db=GnLicenseMetadataDB.from_file(
            Path(args.generated_license_metadata)
        ),
    )

    collector.collect()

    if collector.errors:
        logging.error(
            f"Licenses collection errors: {[str(e) for e in collector.errors]}"
        )
        return -1

    dep_files = []

    def _license_file_reader(path: Path) -> str:
        dep_files.append(path)
        full_path = fuchsia_source_path / path
        logging.debug(f"Reading {full_path}")
        assert (
            full_path.exists()
        ), f"{full_path} does not exist - make sure license() target has correct values"
        assert full_path.is_file(), f"{full_path} is not a file"
        return full_path.read_text()

    # Generate an SPDX file:
    spdx_writer = SpdxWriter.create(
        root_package_name=args.spdx_root_package_name,
        file_reader_func=_license_file_reader,
    )

    for collected_license in collector.unique_licenses:
        spdx_writer.add_license(
            public_package_name=collected_license.public_name,
            license_labels=collected_license.license_files,
            collection_hint=collected_license.debug_hint,
        )

    spdx_output_path = Path(args.spdx_output)
    spdx_writer.save(spdx_output_path)
    logging.info(
        f"Wrote spdx {spdx_output_path} (licenses={len(collector.unique_licenses)} size={spdx_output_path.stat().st_size})"
    )

    # Generate a GN depfile
    dep_file_path = Path(args.dep_file)
    logging.info(f"writing depfile {dep_file_path}")
    with open(dep_file_path, "w") as dep_file:
        dep_file.write(f"{spdx_output_path}:\\\n")
        dep_file.write(
            "\\\n".join([f"    {fuchsia_source_path / p}" for p in dep_files])
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
