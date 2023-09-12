# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_debug_symbols.bzl", "collect_debug_symbols")
load(":providers.bzl", "FuchsiaComponentInfo", "FuchsiaPackageResourcesInfo", "FuchsiaUnitTestComponentInfo")
load(":utils.bzl", "make_resource_struct", "rule_variant", "rule_variants")

def fuchsia_component(name, manifest, deps = None, **kwargs):
    """Creates a Fuchsia component that can be added to a package.

    Args:
        name: The target name.
        manifest: The component manifest file.
        deps: A list of targets that this component depends on.
        **kwargs: Extra attributes to forward to the build rule.
    """
    _fuchsia_component(
        name = name,
        manifest = manifest,
        deps = deps,
        is_driver = False,
        **kwargs
    )

def fuchsia_test_component(name, manifest, deps = None, **kwargs):
    """Creates a Fuchsia component that can be added to a test package.

    Args:
        name: The target name.
        manifest: The component manifest file.
        deps: A list of targets that this component depends on.
        **kwargs: Extra attributes to forward to the build rule.
    """
    _fuchsia_component_test(
        name = name,
        manifest = manifest,
        deps = deps,
        is_driver = False,
        **kwargs
    )

def fuchsia_driver_component(name, manifest, driver_lib, bind_bytecode, deps = None, **kwargs):
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
        **kwargs: Extra attributes to forward to the build rule.
    """
    _fuchsia_component(
        name = name,
        manifest = manifest,
        deps = deps,
        content = {
            driver_lib: "driver/",
            bind_bytecode: "meta/bind/",
        },
        is_driver = True,
        **kwargs
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

    for src, dest in ctx.attr.content.items():
        files_list = src[DefaultInfo].files.to_list()
        if not dest.endswith("/") and len(files_list) > 1:
            fail("To map multiple files in %s, the content mapping %s should end with a slash to indicate a directory." % (ctx.label.name, dest))

        # pkgctl does not play well with paths starting with "/"
        dest = dest.lstrip("/")

        for f in files_list:
            d = dest
            if dest.endswith("/"):
                d = d + f.basename

            resources.append(make_resource_struct(src = f, dest = d))

    return [
        FuchsiaComponentInfo(
            name = component_name,
            manifest = manifest,
            resources = resources,
            is_driver = ctx.attr.is_driver,
            is_test = ctx.attr._variant == "test",
        ),
        collect_debug_symbols(ctx.attr.deps, ctx.attr.content.keys()),
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
        "content": attr.label_keyed_string_dict(
            doc = """A map of dependencies and their destination in the Fuchsia component.
                     If a destination ends with a slash, it is assumed to be a directory""",
            mandatory = False,
        ),
        "manifest": attr.label(
            doc = "The component manifest file",
            allow_single_file = [".cm"],
            mandatory = True,
        ),
        "component_name": attr.string(
            doc = "The name of the package, defaults to the target name",
        ),
        "is_driver": attr.bool(
            doc = "True if this is a driver component",
            default = False,
        ),
    },
)

def _fuchsia_component_for_unit_test_impl(ctx):
    underlying_component = ctx.attr.unit_test[FuchsiaUnitTestComponentInfo].test_component
    return [
        underlying_component[FuchsiaComponentInfo],
        collect_debug_symbols(underlying_component),
    ]

fuchsia_component_for_unit_test = rule_variant(
    variant = "test",
    doc = """Transforms a FuchsiaUnitTestComponentInfo into a test component.""",
    implementation = _fuchsia_component_for_unit_test_impl,
    attrs = {
        "unit_test": attr.label(
            doc = "The unit test to convert into a test component",
            providers = [FuchsiaUnitTestComponentInfo],
        ),
    },
)
