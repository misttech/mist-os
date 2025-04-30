#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Make the Kernel's Assembly Input Bundle

"""

import argparse
import json
import logging
import sys

from assembly import (
    AIBCreator,
    AssemblyInputBundleCreationException,
    KernelInfo,
)

logger = logging.getLogger()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Create an assemblyinput bundle that includes the zircon kernel"
    )

    parser.add_argument(
        "--kernel-image-metadata", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--kernel-image-name", required=True)
    parser.add_argument("--outdir", required=True)
    parser.add_argument(
        "--export-manifest",
        help="Path to write a FINI manifest of the contents of the AIB",
    )
    args = parser.parse_args()

    kernel_metadata = json.load(args.kernel_image_metadata)

    # The build_api_module("images") entry with name "kernel" and type "zbi"
    # is the kernel ZBI to include in the bootable ZBI.  There can be only one.
    [kernel_path] = [
        image["path"]
        for image in kernel_metadata
        if image["name"] == args.kernel_image_name.removesuffix(".zbi")
        and image["type"] == "zbi"
    ]
    kernel = KernelInfo()
    kernel.path = kernel_path

    aib_creator = AIBCreator(args.outdir)
    aib_creator.kernel = kernel

    (
        assembly_input_bundle,
        assembly_config_manifest_path,
        deps,
    ) = aib_creator.build()

    # Write out a fini manifest of the files that have been copied, to create a
    # package or archive that contains all of the files in the bundle.
    if args.export_manifest:
        with open(args.export_manifest, "w") as export_manifest:
            assembly_input_bundle.write_fini_manifest(
                export_manifest, base_dir=args.outdir
            )


if __name__ == "__main__":
    try:
        main()
    except AssemblyInputBundleCreationException:
        logger.exception(
            "A problem occured building the kernel assembly input bundle"
        )
    finally:
        sys.exit()
