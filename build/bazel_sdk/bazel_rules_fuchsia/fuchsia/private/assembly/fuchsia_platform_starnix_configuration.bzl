# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaStarnixConfigInfo")

def _fuchsia_platform_starnix_configuration_impl(ctx):
    return [
        FuchsiaStarnixConfigInfo(
            enabled = ctx.attr.enabled,
        ),
    ]

fuchsia_platform_starnix_configuration = rule(
    doc = """Generates a starnix configuration.""",
    implementation = _fuchsia_platform_starnix_configuration_impl,
    provides = [FuchsiaStarnixConfigInfo],
    attrs = {
        "enabled": attr.bool(
            doc = "Whether starnix support should be included",
        ),
    },
)
