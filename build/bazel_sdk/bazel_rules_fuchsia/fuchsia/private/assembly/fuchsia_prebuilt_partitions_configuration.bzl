# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for instantiating a prebuilt partitions configuration."""

load(":providers.bzl", "FuchsiaAssemblyConfigInfo")

def _fuchsia_prebuilt_partitions_configuration_impl(ctx):
    return [
        DefaultInfo(files = depset(direct = ctx.files.srcs)),
        FuchsiaAssemblyConfigInfo(config = ctx.file.partitions_config),
    ]

fuchsia_prebuilt_partitions_configuration = rule(
    doc = """Instantiates a prebuilt partitions configuration.""",
    implementation = _fuchsia_prebuilt_partitions_configuration_impl,
    attrs = {
        "srcs": attr.label(
            doc = "A filegroup target capturing all prebuilt partition config artifacts.",
            allow_files = True,
            mandatory = True,
        ),
        "partitions_config": attr.label(
            doc = "Relative path of prebuilt partition config file. Must be present within `srcs` as well.",
            allow_single_file = [".json"],
            mandatory = True,
        ),
    },
)
