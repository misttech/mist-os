# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaInputConfigInfo")

# Define input device types option
INPUT_DEVICE_TYPE = struct(
    BUTTON = "button",
    KEYBOARD = "keyboard",
    MOUSE = "mouse",
    TOUCHSCREEN = "touchscreen",
)

def _fuchsia_platform_input_configuration_impl(ctx):
    if ctx.attr.idle_threshold_minutes == "":
        return [
            FuchsiaInputConfigInfo(
                supported_input_devices = ctx.attr.supported_input_devices,
            ),
        ]
    return [
        FuchsiaInputConfigInfo(
            supported_input_devices = ctx.attr.supported_input_devices,
            idle_threshold_minutes = ctx.attr.idle_threshold_minutes,
        ),
    ]

fuchsia_platform_input_configuration = rule(
    doc = """Generates an input configuration.""",
    implementation = _fuchsia_platform_input_configuration_impl,
    provides = [FuchsiaInputConfigInfo],
    attrs = {
        "idle_threshold_minutes": attr.string(
            doc = "How much time has passed since the last user input activity" +
                  "for the system to become idle.",
        ),
        "supported_input_devices": attr.string_list(
            doc = "Options for features that may either be forced on, forced" +
                  "off, or allowed to be either on or off. Features default to disabled.",
            default = [],
        ),
    },
)
