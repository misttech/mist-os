# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_component_manifest.bzl", "fuchsia_component_manifest")
load(":fuchsia_debug_symbols.bzl", "collect_debug_symbols")
load(":providers.bzl", "FuchsiaComponentInfo", "FuchsiaPackageResourcesInfo", "FuchsiaUnitTestComponentInfo")
load(":utils.bzl", "label_name", "make_resource_struct", "rule_variant", "rule_variants")

def _manifest_target(name, manifest_in, tags):
    if manifest_in.endswith(".cml"):
        # We need to compile the cml file
        manifest_target = name + "_" + manifest_in
        fuchsia_component_manifest(
            name = manifest_target,
            src = manifest_in,
            tags = tags + ["manual"],
        )
        return ":{}".format(manifest_target)
    return manifest_in

def fuchsia_component(*, name, manifest, deps = [], tags = ["manual"], **kwargs):
    """Creates a Fuchsia component that can be added to a package.

    Args:
        name: The target name.
        manifest: The component manifest file.
        deps: A list of targets that this component depends on.
        tags: Typical meaning in Bazel. By default this target is manual.
        **kwargs: Extra attributes to forward to the build rule.
    """
    manifest_target = _manifest_target(name, manifest, tags)

    _fuchsia_component(
        name = name,
        manifest = manifest_target,
        deps = deps,
        tags = tags,
        is_driver = False,
        **kwargs
    )

def fuchsia_test_component(*, name, manifest, deps = [], tags = ["manual"], **kwargs):
    """Creates a Fuchsia component that can be added to a test package.

    Args:
        name: The target name.
        manifest: The component manifest file.
        deps: A list of targets that this component depends on.
        tags: Typical meaning in Bazel. By default this target is manual.
        **kwargs: Extra attributes to forward to the build rule.
    """
    manifest_target = _manifest_target(name, manifest, tags)

    _fuchsia_component_test(
        name = name,
        manifest = manifest_target,
        deps = deps,
        tags = tags,
        is_driver = False,
        **kwargs
    )

def fuchsia_driver_component(name, manifest, driver_lib, bind_bytecode, deps = [], tags = [], **kwargs):
    """Creates a Fuchsia component that can be registered as a driver.

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
    manifest_target = _manifest_target(name, manifest, tags)

    _fuchsia_component(
        name = name,
        manifest = manifest_target,
        deps = deps + [
            bind_bytecode,
            driver_lib,
        ],
        tags = tags,
        is_driver = True,
        **kwargs
    )

def _make_fuchsia_component_info(*, component_name, manifest, resources, is_driver, is_test, run_tag):
    return FuchsiaComponentInfo(
        name = component_name,
        manifest = manifest,
        resources = resources,
        is_driver = is_driver,
        is_test = is_test,
        run_tag = run_tag,
    )

def _fuchsia_component_impl(ctx):
    component_name = ctx.attr.component_name or ctx.label.name
    manifest = ctx.file.manifest

    resources = []
    for dep in ctx.attr.deps:
        if FuchsiaPackageResourcesInfo in dep:
            resources += dep[FuchsiaPackageResourcesInfo].resources
        else:
            for mapping in dep[DefaultInfo].default_runfiles.root_symlinks.to_list():
                resources.append(make_resource_struct(src = mapping.target_file, dest = mapping.path))

            for f in dep.files.to_list():
                resources.append(make_resource_struct(src = f, dest = f.short_path))

    return [
        _make_fuchsia_component_info(
            component_name = component_name,
            manifest = manifest,
            resources = resources,
            is_driver = ctx.attr.is_driver,
            is_test = ctx.attr._variant == "test",
            run_tag = label_name(str(ctx.label)),
        ),
        collect_debug_symbols(ctx.attr.deps),
    ]

_fuchsia_component, _fuchsia_component_test = rule_variants(
    variants = (None, "test"),
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
        "manifest": attr.label(
            doc = """The component manifest file

            This attribute can be a fuchsia_component_manifest target or a cml
            file. If a cml file is provided it will be compiled into a cm file.
            If component_name is provided the cm file will inherit that name,
            otherwise it will keep the same basename.

            If you need to have more control over the compilation of the .cm file
            we suggest you create a fuchsia_component_manifest target.
            """,
            allow_single_file = [".cm", ".cml"],
            mandatory = True,
        ),
        "component_name": attr.string(
            doc = "The name of the component, defaults to the target name",
        ),
        "is_driver": attr.bool(
            doc = "True if this is a driver component",
            default = False,
        ),
    },
)

def _fuchsia_component_for_unit_test_impl(ctx):
    underlying_component = ctx.attr.unit_test[FuchsiaUnitTestComponentInfo].test_component
    component_info = underlying_component[FuchsiaComponentInfo]

    return [
        _make_fuchsia_component_info(
            component_name = component_info.name,
            manifest = component_info.manifest,
            resources = component_info.resources,
            is_driver = component_info.is_driver,
            is_test = component_info.is_test,
            run_tag = ctx.attr.run_tag,
        ),
        collect_debug_symbols(underlying_component),
    ]

_fuchsia_component_for_unit_test = rule_variant(
    variant = "test",
    doc = """Transforms a FuchsiaUnitTestComponentInfo into a test component.""",
    implementation = _fuchsia_component_for_unit_test_impl,
    attrs = {
        "unit_test": attr.label(
            doc = "The unit test to convert into a test component",
            providers = [FuchsiaUnitTestComponentInfo],
        ),
        "run_tag": attr.string(
            doc = """A tag used to identify the original component.

            This is most likely going to be the label name for the target which
            created the test component.
            """,
            mandatory = True,
        ),
    },
)

def fuchsia_component_for_unit_test(*, name, tags = ["manual"], **kwargs):
    _fuchsia_component_for_unit_test(
        name = name,
        tags = tags,
        **kwargs
    )
