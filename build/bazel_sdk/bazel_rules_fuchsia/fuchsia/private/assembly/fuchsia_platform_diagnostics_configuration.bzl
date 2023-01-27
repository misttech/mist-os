# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaDiagnosticsConfigInfo")

ARCHIVIST_TYPES = struct(
    BRINGUP = "bringup",
    DEFAULT_SERVICE = "default-service",
    NO_DETECT_SERVICE = "no-detect-service",
    NO_SERVICE = "no-service",
    LOW_MEM = "low-mem-default-service-config"
)

def _fuchsia_platform_diagnostics_configuration_impl(ctx):
    return [
        FuchsiaDiagnosticsConfigInfo(
            archivist = ctx.attr.archivist,
        ),
    ]

fuchsia_platform_diagnostics_configuration = rule(
    doc = """Generates an diagnostics configuration.""",
    implementation = _fuchsia_platform_diagnostics_configuration_impl,
    provides = [FuchsiaDiagnosticsConfigInfo],
    attrs = {
        "archivist": attr.string(
            doc = "A string value for the archivist configuration flavor",
            values = [
                ARCHIVIST_TYPES.BRINGUP,
                ARCHIVIST_TYPES.DEFAULT_SERVICE,
                ARCHIVIST_TYPES.NO_DETECT_SERVICE,
                ARCHIVIST_TYPES.NO_SERVICE,
                ARCHIVIST_TYPES.LOW_MEM
            ],
            default = ARCHIVIST_TYPES.DEFAULT_SERVICE,
        ),
    },
)
