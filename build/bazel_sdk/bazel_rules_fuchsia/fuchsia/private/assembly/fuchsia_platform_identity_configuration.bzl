# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaIdentityConfigInfo")

# Define Feature control option
FEATURE_CONTROL = struct(
    DISABLED = "disabled",
    ALLOWED = "allowed",
    REQUIRED = "required",
)

def _fuchsia_platform_identity_configuration_impl(ctx):
    return [
        FuchsiaIdentityConfigInfo(
            password_pinweaver = ctx.attr.password_pinweaver,
        ),
    ]

fuchsia_platform_identity_configuration = rule(
    doc = """Generates an identity configuration.""",
    implementation = _fuchsia_platform_identity_configuration_impl,
    provides = [FuchsiaIdentityConfigInfo],
    attrs = {
        "password_pinweaver": attr.string(
            doc = "Options for features that may either be forced on, forced" +
                  "off, or allowed to be either on or off. Features default to disabled.",
            default = FEATURE_CONTROL.DISABLED,
            values = [FEATURE_CONTROL.DISABLED, FEATURE_CONTROL.ALLOWED, FEATURE_CONTROL.REQUIRED],
        ),
    },
)
