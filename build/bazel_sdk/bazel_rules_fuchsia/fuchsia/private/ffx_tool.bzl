# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common definition for rules using the ffx tool."""

def get_ffx_assembly_inputs(fuchsia_toolchain):
    """Return the list of inputs needed to run `ffx assembly` commands.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of File instances.
    """

    # NOT: The 'ffx assembly' plugin used by this command requires access to
    # several host tools, and their location will be found by parsing the
    # top-level SDK manifest then each individual tool manifest. Make sure
    # these are listed as proper inputs.
    return [
        fuchsia_toolchain.ffx,
        fuchsia_toolchain.ffx_assembly,
        fuchsia_toolchain.ffx_assembly_fho_meta,
        fuchsia_toolchain.ffx_assembly_manifest,
        fuchsia_toolchain.sdk_manifest,
        fuchsia_toolchain.blobfs,
        fuchsia_toolchain.blobfs_manifest,
        fuchsia_toolchain.cmc,
        fuchsia_toolchain.cmc_manifest,
        fuchsia_toolchain.fvm,
        fuchsia_toolchain.fvm_manifest,
        fuchsia_toolchain.minfs,
        fuchsia_toolchain.minfs_manifest,
        fuchsia_toolchain.zbi,
        fuchsia_toolchain.zbi_manifest,
    ]

def get_ffx_product_bundle_inputs(fuchsia_toolchain):
    """Return the list of inputs needed to run `ffx product` commands.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of File instances.
    """
    return [
        fuchsia_toolchain.ffx,
        fuchsia_toolchain.ffx_product,
        fuchsia_toolchain.ffx_product_fho_meta,
        fuchsia_toolchain.ffx_product_manifest,
        fuchsia_toolchain.sdk_manifest,
    ]
