# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Configuration for the driver framework subsystem
"""

load(":providers.bzl", "FuchsiaDriverFrameworkConfigInfo")

def _fuchsia_platform_driver_framework_configuration_impl(ctx):
    return [
        FuchsiaDriverFrameworkConfigInfo(
            live_usb_enabled = ctx.attr.live_usb_enabled,
            configure_fshost = ctx.attr.configure_fshost,
        ),
    ]

fuchsia_platform_driver_framework_configuration = rule(
    doc = """Generates an driver framework configuration.""",
    implementation = _fuchsia_platform_driver_framework_configuration_impl,
    provides = [FuchsiaDriverFrameworkConfigInfo],
    attrs = {
        "eager_drivers": attr.string_list(
            doc = "A list of drivers to bind 'eagerly'. This turns a driver that normally binds as a fallback driver into a driver that will be bound normally",
        ),
        "disabled_drivers": attr.string_list(
            doc = "A list of drivers by URL. These drivers will not be bound or loaded",
        ),
    },
)
