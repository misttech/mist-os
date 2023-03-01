# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaVirtualizationConfigInfo")

def _fuchsia_platform_virtualization_configuration_impl(ctx):
    return [
        FuchsiaVirtualizationConfigInfo(
            enabled = ctx.attr.enabled,
        ),
    ]

fuchsia_platform_virtualization_configuration = rule(
    doc = """Generates a virtualization configuration.""",
    implementation = _fuchsia_platform_virtualization_configuration_impl,
    provides = [FuchsiaVirtualizationConfigInfo],
    attrs = {
        "enabled": attr.bool(
            doc = "Whether virtualization support should be included",
        ),
    },
)
