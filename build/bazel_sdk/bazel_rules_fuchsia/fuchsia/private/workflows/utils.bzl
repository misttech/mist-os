# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common utilities used for workflows/tasks."""

load("//fuchsia/private:fuchsia_transition.bzl", _with_fuchsia_transition = "with_fuchsia_transition")
load(
    "//fuchsia/private:utils.bzl",
    _alias = "alias",
    _collect_runfiles = "collect_runfiles",
    _flatten = "flatten",
    _label_name = "label_name",
    _normalized_target_name = "normalized_target_name",
    _rule_variants = "rule_variants",
    _wrap_executable = "wrap_executable",
)

alias = _alias
collect_runfiles = _collect_runfiles
flatten = _flatten
label_name = _label_name
normalized_target_name = _normalized_target_name
rule_variants = _rule_variants
with_fuchsia_transition = _with_fuchsia_transition
wrap_executable = _wrap_executable

def full_product_bundle_url(ctx, pb_info):
    """ Returns the full url for the product bundle.

    If the product does not have a version associated with it the sdk version
    will be used. A valid fuchsia toolchain must be registered in the context.

    Args:
      ctx: rule context.
      pb_info: product bundle info.

    Returns:
      URL string.
    """
    version = pb_info.version or ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"].sdk_id
    if not version:
        fail("Cannot find a version in the Fuchsia SDK")

    return "gs://fuchsia/development/{version}/sdk/product_bundles.json#{product}".format(
        version = version,
        product = pb_info.product_name,
    )
