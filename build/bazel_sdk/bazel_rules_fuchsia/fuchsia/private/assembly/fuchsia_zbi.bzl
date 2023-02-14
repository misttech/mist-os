# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for desclaring a ZBI image."""

load(":providers.bzl", "FuchsiaZbiInfo")

# Define Zbi compression format
ZBI_COMPRESSION = struct(
    ZSTD_MAX = "zstd.max",
    ZSTD = "zstd",
)

def _fuchsia_zbi_impl(ctx):
    return [
        FuchsiaZbiInfo(
            zbi_name = ctx.attr.zbi_name,
            compression = ctx.attr.compression,
            postprocessing_script = ctx.file.postprocessing_script,
            postprocessing_args = ctx.attr.postprocessing_args,
        ),
    ]

fuchsia_zbi = rule(
    doc = """Generates a fuchsia zbi image.""",
    implementation = _fuchsia_zbi_impl,
    provides = [FuchsiaZbiInfo],
    attrs = {
        "zbi_name": attr.string(
            doc = "Name of zbi image appeared in image configuration",
            default = "fuchsia",
        ),
        "compression": attr.string(
            doc = "Zbi compression format",
            default = "zstd",
            values = [ZBI_COMPRESSION.ZSTD_MAX, ZBI_COMPRESSION.ZSTD],
        ),
        "postprocessing_script": attr.label(
            doc = "Post-procesing script",
            allow_single_file = True,
            default = None,
        ),
        "postprocessing_args": attr.string_list(
            doc = "Args needed by post-processing script",
            default = [],
        ),
    },
)
