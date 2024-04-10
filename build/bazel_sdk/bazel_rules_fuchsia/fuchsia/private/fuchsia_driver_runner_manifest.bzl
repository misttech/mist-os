# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageResourcesInfo")
load(":fuchsia_component_manifest.bzl", "fuchsia_component_manifest")

def _gen_fuchsia_driver_runner_cml_impl(ctx):
    cml_file = ctx.actions.declare_file((ctx.attr.component_name or ctx.label.name) + ".cml")
    program = None
    bind = None

    for resource in ctx.attr.driver_lib[FuchsiaPackageResourcesInfo].resources + ctx.attr.bind_bytecode[FuchsiaPackageResourcesInfo].resources:
        dest = resource.dest
        if dest.startswith("driver/"):
            if dest == "driver/compat.so":
                continue
            if program:
                fail("Driver runner manifests can only contain a single driver entry.")
            program = dest
        if dest.startswith("meta/bind/"):
            if bind:
                fail("Driver runner manifests can only contain a single bind library.")
            bind = dest

    if not program:
        fail("Driver runner manifests must contain a driver binary")
    if not bind:
        fail("Drivers runner manifests must contain a bind library")

    manifest = {
        "program": {
            "runner": "driver",
            "binary": program,
            "bind": bind,
            "default_dispatcher_opts": ctx.attr.default_dispatcher_opts,
        },
        "use": [],
    }

    if ctx.attr.fallback:
        manifest["program"]["fallback"] = "true"

    if ctx.attr.colocate:
        manifest["program"]["colocate"] = "true"

    if ctx.attr.root_resource:
        manifest["use"].append({"protocol": "fuchsia.boot.RootResource"})

    if ctx.attr.uses_profiles:
        manifest["use"].append({"protocol": "fuchsia.scheduler.ProfileProvider"})

    if ctx.attr.uses_sysmem:
        manifest["use"].append({"protocol": "fuchsia.sysmem.Allocator"})

    if ctx.attr.uses_boot_args:
        manifest["use"].append({"protocol": "fuchsia.boot.Arguments"})

    if ctx.attr.default_dispatcher_opts:
        manifest["program"]["default_dispatcher_opts"] = ctx.attr.default_dispatcher_opts
    if ctx.attr.default_dispatcher_scheduler_role:
        manifest["program"]["default_dispatcher_scheduler_role"] = ctx.attr.default_dispatcher_scheduler_role

    ctx.actions.write(cml_file, content = json.encode_indent(manifest))
    return DefaultInfo(
        files = depset([cml_file]),
    )

_gen_fuchsia_driver_runner_cml = rule(
    implementation = _gen_fuchsia_driver_runner_cml_impl,
    doc = "Generates a cml file from the given inputs",
    attrs = {
        "component_name": attr.string(
            doc = """The name of the component this cml file belongs to.

            This will be the name of the file with the .cml extension. Defaults
            to 'name'.
            """,
        ),
        "driver_lib": attr.label(
            doc = "The driver library to launch",
            mandatory = True,
        ),
        "bind_bytecode": attr.label(
            doc = "The driver bind bytecode",
            mandatory = True,
        ),
        "fallback": attr.bool(
            doc = "Whether or not the driver is a fallback driver",
            default = False,
        ),
        "colocate": attr.bool(
            doc = "Whether or not the driver should be colocated",
            default = False,
        ),
        "root_resource": attr.bool(
            doc = "Whether or not to give the driver access to the root resource",
            default = False,
        ),
        "uses_profiles": attr.bool(
            doc = "Whether or not to give the driver access to fuchsia.scheduler.ProfileProvider",
            default = False,
        ),
        "uses_sysmem": attr.bool(
            doc = "Whether or not to give the driver access to fuchsia.sysmem.Allocator",
            default = False,
        ),
        "uses_boot_args": attr.bool(
            doc = "Whether or not to give the driver access to fuchsia.boot.Arguments",
            default = False,
        ),
        "default_dispatcher_opts": attr.string_list(
            doc = "A list of default dispatcher options for the driver",
            default = ["allow_sync_calls"],
        ),
        "default_dispatcher_scheduler_role": attr.string(
            doc = """If set, then the default dispatcher for the driver will be
            created with this scheduler role""",
        ),
    },
)

def fuchsia_driver_runner_manifest(name, **kwargs):
    """Creates a component manifest suitable for the driver runner.

    This rule should not be used directly. Instead, users should create a fuchsia_driver_component
    which will create the component manifest if one is not provided.

    Args:
        name: The target name,
        **kwargs: The args to pass to the cml file generator. See _gen_fuchsia_driver_runner_cml
           for specifics.
    """
    _gen_fuchsia_driver_runner_cml(
        # Note: This name is used by tests, if it is changed the tests will fail.
        name = name + "_gen_cml",
        **kwargs
    )
    fuchsia_component_manifest(
        name = name,
        src = ":" + name + "_gen_cml",
    )
