# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_component_manifest.bzl", "ensure_compiled_component_manifest")
load(":fuchsia_debug_symbols.bzl", "collect_debug_symbols")
load(
    ":providers.bzl",
    "FuchsiaComponentInfo",
    "FuchsiaComponentManifestInfo",
    "FuchsiaPackageResourcesInfo",
)
load(":utils.bzl", "label_name", "make_resource_struct")

def _manifest_target(name, manifest_in, tags, testonly):
    target_name = name + "_ensure_compiled_manifest"
    ensure_compiled_component_manifest(
        name = target_name,
        dep = manifest_in,
        testonly = testonly,
        tags = tags + ["manual"],
    )
    return target_name

def fuchsia_component(
        *,
        name,
        manifest,
        moniker = "/core/ffx-laboratory:{COMPONENT_NAME}",
        deps = [],
        tags = ["manual"],
        **kwargs):
    """Creates a Fuchsia component that can be added to a package.

    Args:
        name: The target name.
        manifest: The component manifest file.
            This attribute can be a fuchsia_component_manifest target or a cml
            file. If a cml file is provided it will be compiled into a cm file.
            If component_name is provided the cm file will inherit that name,
            otherwise it will keep the same basename.

            If you need to have more control over the compilation of the .cm file
            we suggest you create a fuchsia_component_manifest target.
        moniker: The moniker to run the component under.
            Defaults to "/core/ffx-laboratory:{COMPONENT_NAME}".
        deps: A list of targets that this component depends on.
        tags: Typical meaning in Bazel. By default this target is manual.
        **kwargs: Extra attributes to forward to the build rule.
    """
    manifest_target = _manifest_target(name, manifest, tags, testonly = False)

    _fuchsia_component(
        name = name,
        compiled_manifest = manifest_target,
        moniker = moniker,
        deps = deps,
        tags = tags,
        **kwargs
    )

def fuchsia_test_component(*, name, manifest, deps = [], tags = ["manual"], **kwargs):
    """Creates a Fuchsia component that can be added to a test package.

    See fuchsia_component for more information.

    Args:
        name: The target name.
        manifest: The component manifest file.
        deps: A list of targets that this component depends on.
        tags: Typical meaning in Bazel. By default this target is manual.
        **kwargs: Extra attributes to forward to the build rule.
    """
    manifest_target = _manifest_target(name, manifest, tags, testonly = True)

    _fuchsia_component(
        name = name,
        compiled_manifest = manifest_target,
        deps = deps,
        tags = tags,
        is_test = True,
        testonly = True,
        **kwargs
    )

def fuchsia_driver_component(name, manifest, driver_lib, bind_bytecode, deps = [], tags = [], **kwargs):
    """Creates a Fuchsia component that can be registered as a driver.

    See fuchsia_component for more information.

    Args:
        name: The target name.
        manifest: The component manifest file.
        driver_lib: The shared library that will be registered with the driver manager.
           This file will end up in /driver/<lib_name> and should match what is listed
           in the manifest. See https://fuchsia.dev/fuchsia-src/concepts/components/v2/driver_runner
           for more details.
        bind_bytecode: The driver bind bytecode needed for binding the driver.
        deps: A list of targets that this component depends on.
        tags: Typical meaning in Bazel. By default this target is manual.
        **kwargs: Extra attributes to forward to the build rule.
    """
    manifest_target = _manifest_target(name, manifest, tags, testonly = False)

    _fuchsia_component(
        name = name,
        compiled_manifest = manifest_target,
        deps = deps + [
            bind_bytecode,
            driver_lib,
        ],
        tags = tags,
        is_driver = True,
        **kwargs
    )

def _make_fuchsia_component_providers(*, component_name, manifest, resources, is_driver, is_test, moniker, run_tag):
    return [
        FuchsiaComponentInfo(
            name = component_name,
            manifest = manifest,
            resources = resources,
            is_driver = is_driver,
            is_test = is_test,
            moniker = moniker,
            run_tag = run_tag,
        ),
        FuchsiaPackageResourcesInfo(resources = [
            make_resource_struct(
                src = manifest,
                dest = "meta/{}".format(manifest.basename),
            ),
        ]),
    ]

def _fuchsia_component_impl(ctx):
    component_name = ctx.attr.component_name or ctx.attr.compiled_manifest[FuchsiaComponentManifestInfo].component_name
    manifest = ctx.attr.compiled_manifest[FuchsiaComponentManifestInfo].compiled_manifest

    resources = []
    for dep in ctx.attr.deps:
        if FuchsiaPackageResourcesInfo in dep:
            resources += dep[FuchsiaPackageResourcesInfo].resources
        else:
            for mapping in dep[DefaultInfo].default_runfiles.root_symlinks.to_list():
                resources.append(make_resource_struct(src = mapping.target_file, dest = mapping.path))

            for f in dep.files.to_list():
                resources.append(make_resource_struct(src = f, dest = f.short_path))

    return _make_fuchsia_component_providers(
        component_name = component_name,
        manifest = manifest,
        resources = resources,
        is_driver = ctx.attr.is_driver,
        is_test = ctx.attr.is_test,
        moniker = ctx.attr.moniker.format(COMPONENT_NAME = component_name),
        run_tag = label_name(str(ctx.label)),
    ) + [
        collect_debug_symbols(ctx.attr.deps),
    ]

_fuchsia_component = rule(
    doc = """Creates a Fuchsia component which can be added to a package

This rule will take a component manifest and compile it into a form that
is suitable to be included in a package. The component can include any
number of dependencies which will be included in the final package.
""",
    implementation = _fuchsia_component_impl,
    attrs = {
        "deps": attr.label_list(
            doc = "A list of targets that this component depends on",
        ),
        "moniker": attr.string(
            doc = "The moniker to run non-test, non-driver, and non-session components under.",
        ),
        "compiled_manifest": attr.label(
            doc = """The component manifest file

            This attribute can be a fuchsia_component_manifest target or a cml
            file. If a cml file is provided it will be compiled into a cm file.
            If component_name is provided the cm file will inherit that name,
            otherwise it will keep the same basename.

            If you need to have more control over the compilation of the .cm file
            we suggest you create a fuchsia_component_manifest target.
            """,
            providers = [FuchsiaComponentManifestInfo],
            mandatory = True,
        ),
        "component_name": attr.string(
            doc = """The name of the component, defaults to the component manifests name.
            Note: This value will override any component_name values that were
            set on the component manifest.""",
        ),
        "is_driver": attr.bool(
            doc = "True if this is a driver component",
            default = False,
        ),
        "is_test": attr.bool(
            doc = "True if this is a test component",
            default = False,
        ),
    },
)
