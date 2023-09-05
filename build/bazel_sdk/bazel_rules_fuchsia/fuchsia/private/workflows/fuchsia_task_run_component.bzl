# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Runs components, tests components, or register drivers within a package."""

load(":fuchsia_task.bzl", "fuchsia_task_rule")
load(":providers.bzl", "FuchsiaComponentInfo", "FuchsiaPackageInfo")

def _fuchsia_task_run_component_impl(ctx, make_fuchsia_task):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    repo = ctx.attr.repository
    package = ctx.attr.package[FuchsiaPackageInfo].package_name
    component = ctx.attr.component[FuchsiaComponentInfo]
    component_name = component.name
    manifest = component.manifest.basename
    url = "fuchsia-pkg://%s/%s#meta/%s" % (repo, package, manifest)
    moniker = ctx.attr.moniker or "/core/ffx-laboratory:%s" % component_name
    if component.is_driver:
        args = [
            "--ffx",
            sdk.ffx,
            "--url",
            url,
        ]
        if ctx.attr.disable_repository:
            disable_url = "fuchsia-pkg://%s/%s#meta/%s" % (ctx.attr.disable_repository, package, manifest)
            args += [
                "--disable-url",
                disable_url,
            ]
        return make_fuchsia_task(
            ctx.attr._register_driver_tool,
            args,
        )
    elif component.is_test:
        return make_fuchsia_task(
            ctx.attr._run_test_component_tool,
            [
                "--ffx",
                sdk.ffx,
                "--url",
                url,
            ],
        )
    else:
        return make_fuchsia_task(
            ctx.attr._run_component_tool,
            [
                "--ffx",
                sdk.ffx,
                "--moniker",
                moniker,
                "--url",
                url,
            ],
        )

(
    _fuchsia_task_run_component,
    _fuchsia_task_run_component_for_test,
    fuchsia_task_run_component,
) = fuchsia_task_rule(
    implementation = _fuchsia_task_run_component_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "repository": attr.string(
            doc = "The repository that has the published package.",
            mandatory = True,
        ),
        "package": attr.label(
            doc = "The package containing the component.",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "moniker": attr.string(
            doc = "The moniker to run the component in. Only used for non-test non-driver components.",
        ),
        "component": attr.label(
            doc = "The component to run.",
            providers = [FuchsiaComponentInfo],
            mandatory = True,
        ),
        "disable_repository": attr.string(
            doc = "The repository that contains the pre-existed driver we want to disable. This is only used in driver workflow now.",
        ),
        "_register_driver_tool": attr.label(
            doc = "The tool used to run components",
            default = "//fuchsia/tools:register_driver",
        ),
        "_run_test_component_tool": attr.label(
            doc = "The tool used to run components",
            default = "//fuchsia/tools:run_test_component",
        ),
        "_run_component_tool": attr.label(
            doc = "The tool used to run components",
            default = "//fuchsia/tools:run_component",
        ),
    },
)
