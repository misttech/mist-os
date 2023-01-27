# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaDevelopmentSupportConfigInfo")

def _fuchsia_platform_development_support_configuration_impl(ctx):
    return [
        FuchsiaDevelopmentSupportConfigInfo(
            enabled = ctx.attr.enabled,
        ),
    ]

fuchsia_platform_development_support_configuration = rule(
    doc = """Generates an development_support configuration.""",
    implementation = _fuchsia_platform_development_support_configuration_impl,
    provides = [FuchsiaDevelopmentSupportConfigInfo],
    attrs = {
        "enabled": attr.bool(
            doc = "A bool value whether development_support is enabled",
        ),
    },
)
