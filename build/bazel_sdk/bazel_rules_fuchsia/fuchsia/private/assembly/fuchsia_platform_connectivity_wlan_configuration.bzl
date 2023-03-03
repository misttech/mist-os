# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaConnectivityWlanConfigInfo")

def _fuchsia_platform_connectivity_wlan_configuration_impl(ctx):
    return [
        FuchsiaConnectivityWlanConfigInfo(
            legacy_privacy_support = ctx.attr.legacy_privacy_support,
        ),
    ]

fuchsia_platform_connectivity_wlan_configuration = rule(
    doc = """Generates an connectivity configuration.""",
    implementation = _fuchsia_platform_connectivity_wlan_configuration_impl,
    provides = [FuchsiaConnectivityWlanConfigInfo],
    attrs = {
        "legacy_privacy_support": attr.bool(
            doc = "A bool value for legacy_privacy_support of wlan",
        ),
    },
)
