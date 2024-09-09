# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common definition for rules using the ffx tool."""

# TODO(https://fxbug.dev/287355615): This logic actually belongs in the IDK, not the
# SDK. The SDK should just read this data from the IDK's json manifests.
#
# If you are considering adding another method here, please ping
# https://fxbug.dev/287355615, and we can potentially prioritize fixing this the Right
# Way™.

def _to_quoted_comma_separated_list(args):
    """Return a list of strings as a shell-quoted, comma-separated string."""
    return "\"%s\"" % ",".join(args)

def get_ffx_assembly_inputs(fuchsia_toolchain):
    """Return the list of inputs needed to run `ffx assembly` commands.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of File instances.
    """

    # NOT: The 'ffx assembly' plugin used by this command requires access to
    # several host tools, and their location will be passed with explicit
    # overrides (see get_ffx_assembly_args()).
    return [
        fuchsia_toolchain.ffx,
        fuchsia_toolchain.ffx_assembly,
        fuchsia_toolchain.ffx_assembly_fho_meta,
        fuchsia_toolchain.blobfs,
        fuchsia_toolchain.cmc,
        fuchsia_toolchain.fvm,
        fuchsia_toolchain.minfs,
        fuchsia_toolchain.zbi,
    ]

def get_ffx_assembly_args(fuchsia_toolchain):
    """Return the start of a command line used to call `ffx assembly`.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of string or File instances.
    """
    return [
        fuchsia_toolchain.ffx_assembly.path,
        "--config",
        _to_quoted_comma_separated_list([
            "assembly_enabled=true",
            "sdk.overrides.assembly=" + fuchsia_toolchain.ffx_assembly.path,
            "sdk.overrides.blobfs=" + fuchsia_toolchain.blobfs.path,
            "sdk.overrides.cmc=" + fuchsia_toolchain.cmc.path,
            "sdk.overrides.fvm=" + fuchsia_toolchain.fvm.path,
            "sdk.overrides.minfs=" + fuchsia_toolchain.minfs.path,
            "sdk.overrides.zbi=" + fuchsia_toolchain.zbi.path,
        ]),
    ]

def get_ffx_product_inputs(fuchsia_toolchain):
    """Return the list of inputs needed to run `ffx product` commands.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of File instances.
    """
    return [
        fuchsia_toolchain.ffx_product,
        fuchsia_toolchain.ffx_product_fho_meta,
        fuchsia_toolchain.blobfs,
    ]

def get_ffx_product_args(fuchsia_toolchain):
    return [
        fuchsia_toolchain.ffx_product.path,
        "--config",
        _to_quoted_comma_separated_list([
            "product.experimental=true",
            "sdk.overrides.blobfs=" + fuchsia_toolchain.blobfs.path,
        ]),
    ]

def get_ffx_scrutiny_inputs(fuchsia_toolchain):
    """Return the list of inputs needed to run `ffx scrutiny` commands.

    Args:
      fuchsia_toolchain: A fuchsia_toolchain() instance used to locate
         all host tools, including the 'ffx' one.
    Returns:
      A list of File instances.
    """
    return [
        fuchsia_toolchain.ffx_scrutiny,
        fuchsia_toolchain.ffx_scrutiny_fho_meta,
    ]

def get_ffx_scrutiny_args(fuchsia_toolchain):
    return [
        fuchsia_toolchain.ffx_scrutiny.path,
    ]
