# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for license aggregation in SPDX generation."""

load(
    "@rules_license//rules:gather_licenses_info.bzl",
    "gather_licenses_info",
    "write_licenses_info",
)
load(
    "@rules_license//rules:providers.bzl",
    "TransitiveLicensesInfo",
)

# Debugging verbosity. Set to >0 for debugging
_VERBOSITY = 0

def _debug(loglevel, msg):
    if _VERBOSITY > loglevel:
        print(msg)  # buildifier: disable=print

def _fuchsia_licenses_spdx_impl(ctx):
    _debug(0, "_fuchsia_licenses_spdx_impl")

    license_infos = []
    license_files = []
    if TransitiveLicensesInfo in ctx.attr.target:
        license_infos = ctx.attr.target[TransitiveLicensesInfo].licenses.to_list()
    for info in license_infos:
        license_files.append(info.license_text)
    _debug(0, "Found %s license infos: %s" % (len(license_infos), license_infos))

    licenses_used_file = ctx.actions.declare_file("%s.licenses_used.json" % ctx.attr.name)

    write_licenses_info(
        ctx,
        deps = [ctx.attr.target],
        json_out = licenses_used_file,
    )

    spdx_output = ctx.actions.declare_file(ctx.attr.name)

    root_package_name = ctx.attr.root_package_name
    if not root_package_name:
        root_package_name = ctx.attr.target.label.name

    ctx.actions.run(
        progress_message = "Generating SPDX from %s into %s" %
                           (licenses_used_file.path, spdx_output.path),
        inputs = [licenses_used_file] + license_files,
        outputs = [spdx_output],
        executable = ctx.executable._generate_licenses_spdx_tool,
        arguments = [
            "--licenses_used=%s" % licenses_used_file.path,
            "--spdx_output=%s" % spdx_output.path,
            "--root_package_name=%s" % root_package_name,
            "--root_package_homepage=%s" % ctx.attr.root_package_homepage,
            "--document_namespace=%s" % ctx.attr.document_namespace,
            "--licenses_cross_refs_base_url=%s" % ctx.attr.licenses_cross_refs_base_url,
        ],
    )

    return [DefaultInfo(files = depset([spdx_output]))]

fuchsia_licenses_spdx = rule(
    doc = """
Produces a licenses spdx file for the given target.

This rule generates a licenses SPDX json file for all
@rules_license://rules:license declarations that the given
target depends on.

The SPDX json conforms with:
https://github.com/spdx/spdx-spec/blob/master/schemas/spdx-schema.json
""",
    implementation = _fuchsia_licenses_spdx_impl,
    attrs = {
        "target": attr.label(
            doc = "The target to aggregate the licenses from.",
            mandatory = True,
            aspects = [gather_licenses_info],
        ),
        "root_package_name": attr.string(
            doc = """The name of the SPDX root package.
If absent, the target's name is used instead.""",
        ),
        "root_package_homepage": attr.string(
            doc = """The homepage of the SPDX root package.""",
        ),
        "document_namespace": attr.string(
            doc = "A unique namespace url for the SPDX references in the doc",
            mandatory = True,
        ),
        "licenses_cross_refs_base_url": attr.string(
            doc = "Base URL for license paths that are local files",
            mandatory = True,
        ),
        "_generate_licenses_spdx_tool": attr.label(
            executable = True,
            cfg = "exec",
            default = "//fuchsia/tools/licenses:generate_licenses_spdx",
        ),
    },
)
