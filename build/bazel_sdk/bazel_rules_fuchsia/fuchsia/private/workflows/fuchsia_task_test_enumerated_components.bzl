# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Enumerates all components within a test package and runs each of them as test components."""

load(":fuchsia_task.bzl", "fuchsia_task_rule")
load(":providers.bzl", "FuchsiaPackageInfo")

def _fuchsia_task_test_enumerated_components_impl(ctx, make_fuchsia_task):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    repo = ctx.attr.repository
    package = getattr(ctx.attr.package[FuchsiaPackageInfo], "package_name", "{{PACKAGE_NAME}}")
    package_manifest = ctx.attr.package[FuchsiaPackageInfo].package_manifest
    package_archive = ctx.attr.package[FuchsiaPackageInfo].far_file
    url = "fuchsia-pkg://%s/%s#{{META_COMPONENT}}" % (repo, package)
    args = [
        "--ffx-test",
        sdk.ffx_test,
        "--ffx-package",
        sdk.ffx_package,
        "--url",
        url,
        "--package-manifest",
        package_manifest,
        "--package-archive",
        package_archive,
        "--match-component-name",
        ctx.attr.component_name_filter,
    ]
    if ctx.attr.test_realm:
        args += [
            "--realm",
            ctx.attr.test_realm,
        ]
    return make_fuchsia_task(
        ctx.attr._test_enumerated_components_tool,
        args,
        runfiles = [
            sdk.ffx_package_fho_meta,
            sdk.ffx_package_manifest,
            sdk.ffx_test_fho_meta,
            sdk.ffx_test_manifest,
        ],
    )

(
    _fuchsia_task_test_enumerated_components,
    _fuchsia_task_test_enumerated_components_for_test,
    fuchsia_task_test_enumerated_components,
) = fuchsia_task_rule(
    implementation = _fuchsia_task_test_enumerated_components_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "component_name_filter": attr.string(
            doc = "A regex filter allowlist applied to component names; used to filter components for testing.",
            default = ".+",
        ),
        "repository": attr.string(
            doc = "The repository that has the published package.",
            mandatory = True,
        ),
        "package": attr.label(
            doc = "The package containing the components to enumerate and test.",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "test_realm": attr.string(
            doc = "Specify --realm to `ffx test run`.",
        ),
        "_test_enumerated_components_tool": attr.label(
            doc = "The tool used to enumerate and test all components",
            default = "//fuchsia/tools:test_enumerated_components",
        ),
    },
)
