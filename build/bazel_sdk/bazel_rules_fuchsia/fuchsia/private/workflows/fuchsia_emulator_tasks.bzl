# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""ffx emu invokations as workflow tasks."""

load(":providers.bzl", "FuchsiaProductBundleInfo")
load("//fuchsia/private:providers.bzl", "FuchsiaEmulatorInfo", "FuchsiaLocalPackageRepositoryInfo")
load(":fuchsia_task_ffx.bzl", "ffx_task_rule")
load(":utils.bzl", "full_product_bundle_url")
load(
    ":fuchsia_workflow.bzl",
    "fuchsia_workflow",
)

# buildifier: disable=function-docstring
def fuchsia_task_start_emulator(name, emulator, product_bundle, **kwargs):
    start_name = name + "_emulator"
    register_name = name + "_register_product_bundle"

    _fuchsia_task_start_emulator(
        name = start_name,
        emulator = emulator,
        product_bundle = product_bundle,
        **kwargs
    )

    fuchsia_task_emulator_register_repository(
        name = register_name,
        emulator = emulator,
        repository = product_bundle,
        aliases = ["fuchsia.com", "chromium.org"],
    )

    fuchsia_workflow(
        name = name,
        sequence = [
            start_name,
            register_name,
        ],
    )

def _fuchsia_task_start_emulator_impl(ctx, _make_ffx_task):
    pb_info = ctx.attr.product_bundle[FuchsiaProductBundleInfo]
    return _make_ffx_task(
        prepend_args = [
            "emu",
            "start",
            full_product_bundle_url(ctx, pb_info),
            "--name",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
        ] + ctx.attr.emulator[FuchsiaEmulatorInfo].launch_options,
    )

# buildifier: disable=unused-variable
(
    __fuchsia_task_start_emulator,
    _fuchsia_task_start_emulator_for_test,
    _fuchsia_task_start_emulator,
) = ffx_task_rule(
    doc = """Start an emulator with a product bundle.""",
    implementation = _fuchsia_task_start_emulator_impl,
    attrs = {
        "emulator": attr.label(
            doc = "The emulator that we are stopping",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
        "product_bundle": attr.label(
            doc = "The product bundle to use to start the emulator.",
            providers = [[FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_stop_emulator_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "emu",
            "stop",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
        ],
    )

# buildifier: disable=unused-variable
(
    _fuchsia_task_stop_emulator,
    _fuchsia_task_stop_emulator_for_test,
    fuchsia_task_stop_emulator,
) = ffx_task_rule(
    doc = """Stop an emulator with a given name.""",
    implementation = _fuchsia_task_stop_emulator_impl,
    attrs = {
        "emulator": attr.label(
            doc = "The emulator that we are stopping",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_reboot_emulator_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "--target",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
            "target",
            "reboot",
        ],
    )

(
    _fuchsia_task_reboot_emulator,
    _fuchsia_task_reboot_emulator_for_test,
    fuchsia_task_reboot_emulator,
) = ffx_task_rule(
    doc = """Attempts to reboot the emulator.""",
    implementation = _fuchsia_task_reboot_emulator_impl,
    attrs = {
        "emulator": attr.label(
            doc = "The emulator that we are stopping",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_make_default_emulator_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "target",
            "default",
            "set",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
        ],
    )

(
    _fuchsia_task_make_default_emulator,
    _fuchsia_task_make_default_emulator_for_test,
    fuchsia_task_make_default_emulator,
) = ffx_task_rule(
    doc = """Makes the emulator the default.""",
    implementation = _fuchsia_task_make_default_emulator_impl,
    attrs = {
        "emulator": attr.label(
            doc = "The emulator.",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_emulator_wait_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "--target",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
            "target",
            "wait",
        ],
    )

(
    _fuchsia_task_emulator_wait,
    _fuchsia_task_emulator_wait_for_test,
    fuchsia_task_emulator_wait,
) = ffx_task_rule(
    doc = """Waits for the emulator to come online.""",
    implementation = _fuchsia_task_emulator_wait_impl,
    attrs = {
        "emulator": attr.label(
            doc = "The emulator to register with",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_emulator_register_repository_impl(ctx, _make_ffx_task):
    if FuchsiaLocalPackageRepositoryInfo in ctx.attr.repository:
        repo = ctx.attr.repository[FuchsiaLocalPackageRepositoryInfo].repo_name
    elif FuchsiaProductBundleInfo in ctx.attr.repository:
        repo = ctx.attr.repository[FuchsiaProductBundleInfo].repository
    else:
        fail("Only product bundles and local repositories are supported at this time.")

    aliases = []
    for alias in ctx.attr.aliases:
        aliases.extend(["--alias", alias])

    return _make_ffx_task(
        prepend_args = [
            "target",
            "repository",
            "register",
            "-r",
            repo,
        ] + aliases,
    )

(
    _fuchsia_task_emulator_register_repository,
    _fuchsia_task_emulator_register_repository_for_test,
    fuchsia_task_emulator_register_repository,
) = ffx_task_rule(
    doc = """Registers a package server with the emulator.""",
    implementation = _fuchsia_task_emulator_register_repository_impl,
    attrs = {
        "repository": attr.label(
            doc = "The repository that is being controlled",
            providers = [[FuchsiaLocalPackageRepositoryInfo], [FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
        "emulator": attr.label(
            doc = "The emulator to register with",
            providers = [[FuchsiaEmulatorInfo]],
            mandatory = True,
        ),
        "aliases": attr.string_list(
            doc = "The list of aliases for this repository",
            default = [],
        ),
    },
)
