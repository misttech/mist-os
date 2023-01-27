# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaConnectivityConfigInfo", "FuchsiaConnectivityWlanConfigInfo")

def _fuchsia_platform_connectivity_configuration_impl(ctx):
    return [
        FuchsiaConnectivityConfigInfo(
            wlan = ctx.attr.wlan,
        ),
    ]

fuchsia_platform_connectivity_configuration = rule(
    doc = """Generates an connectivity configuration.""",
    implementation = _fuchsia_platform_connectivity_configuration_impl,
    provides = [FuchsiaConnectivityConfigInfo],
    attrs = {
        "wlan": attr.label(
            providers = [FuchsiaConnectivityWlanConfigInfo],
            doc = "A bool value for legacy_privacy_support of wlan",
        ),
    },
)
