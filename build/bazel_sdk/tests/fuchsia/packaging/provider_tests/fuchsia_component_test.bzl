# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("@fuchsia_sdk//fuchsia:defs.bzl", "fuchsia_component", "fuchsia_component_manifest", "fuchsia_driver_component", "fuchsia_package")
load("@fuchsia_sdk//fuchsia:private_defs.bzl", "FuchsiaComponentInfo", "FuchsiaPackageInfo")
load("//test_utils:make_file.bzl", "make_fake_component_manifest", "make_file")

def _local_name(name):
    """ Returns a name scoped to this file to avoid conflicts when putting into a BUILD file"""
    return "fuchsia_component_test_suite-{name}".format(name = name)

def _local_target(name):
    return ":{}".format(_local_name(name))

## Test rule definitions
def _provider_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    component_info = target_under_test[FuchsiaComponentInfo]

    asserts.equals(
        env,
        ctx.attr.component_name,
        component_info.name,
    )

    asserts.equals(
        env,
        ctx.attr.run_tag,
        component_info.run_tag,
    )

    asserts.equals(
        env,
        ctx.attr.is_driver,
        component_info.is_driver,
    )

    asserts.equals(
        env,
        component_info.manifest.basename,
        ctx.attr.manifest_basename,
    )

    return analysistest.end(env)

_provider_test = analysistest.make(
    _provider_test_impl,
    attrs = {
        "component_name": attr.string(),
        "run_tag": attr.string(),
        "is_driver": attr.bool(),
        "manifest_basename": attr.string(),
    },
)

def _check_component_name_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    packaged_components = target_under_test[FuchsiaPackageInfo].packaged_components

    # We only support a single component for this specific test
    asserts.equals(
        env,
        len(packaged_components),
        1,
    )

    component_info = packaged_components[0].component_info

    asserts.equals(
        env,
        component_info.name,
        ctx.attr.component_name,
    )

    return analysistest.end(env)

_check_component_name_test = analysistest.make(
    _check_component_name_test_impl,
    attrs = {
        "component_name": attr.string(),
    },
)

def _test_provider():
    make_fake_component_manifest(
        name = _local_name("manifest_component"),
        component_name = "component",
        tags = ["manual"],
    )

    make_fake_component_manifest(
        name = _local_name("manifest_driver"),
        component_name = "driver",
        tags = ["manual"],
    )

    fuchsia_component(
        name = _local_name("component"),
        tags = ["manual"],
        component_name = "component",
        manifest = _local_target("manifest_component"),
    )

    make_file(
        name = _local_name("lib"),
        filename = "driver.so",
        content = "",
    )

    make_file(
        name = _local_name("bind"),
        filename = "bind",
        content = "",
    )

    fuchsia_driver_component(
        name = _local_name("driver"),
        tags = ["manual"],
        component_name = "driver",
        manifest = _local_target("manifest_driver"),
        driver_lib = _local_target("lib"),
        bind_bytecode = _local_target("bind"),
    )

    _provider_test(
        name = "test_component_providers",
        target_under_test = _local_target("component"),
        component_name = "component",
        is_driver = False,
        manifest_basename = "component.cm",
        run_tag = _local_name("component"),
    )

    _provider_test(
        name = "test_driver_component_providers",
        target_under_test = _local_target("driver"),
        component_name = "driver",
        is_driver = True,
        manifest_basename = "driver.cm",
        run_tag = _local_name("driver"),
    )

def _test_setting_component_names():
    # 1) check that component_name is taken from the component_manifest's filename
    # if nothing else is specificed
    fuchsia_component_manifest(
        name = "component_manifest_for_component_name_foo",
        src = "meta/foo.cml",
    )
    fuchsia_component(
        name = "component_without_name_specified_named_foo",
        manifest = ":component_manifest_for_component_name_foo",
    )
    fuchsia_package(
        name = "package_to_wrap_component_foo",
        components = [
            ":component_without_name_specified_named_foo",
        ],
        fuchsia_api_level = "HEAD",
    )
    _check_component_name_test(
        name = "check_component_name_taken_from_component_manifest_file_name",
        target_under_test = ":package_to_wrap_component_foo",
        component_name = "foo",
    )

    # 2) check that the component name is taken from the component manifest
    # component_name if not specified on the component
    fuchsia_component_manifest(
        name = "component_manifest_for_component_name_component_A",
        src = "meta/foo.cml",
        component_name = "component_A",
    )
    fuchsia_component(
        name = "component_without_name_specified_named_component_A",
        manifest = ":component_manifest_for_component_name_component_A",
    )
    fuchsia_package(
        name = "package_to_wrap_component_A",
        components = [
            ":component_without_name_specified_named_component_A",
        ],
        fuchsia_api_level = "HEAD",
    )
    _check_component_name_test(
        name = "check_component_name_taken_from_component_manifest_component_name_attribute",
        target_under_test = ":package_to_wrap_component_A",
        component_name = "component_A",
    )

    # 3) check that the component name is taken from the component's
    # component_name if specified
    make_file(
        name = "generated_component_manifest_comnponent_C",
        filename = "component_C.cm",
        tags = ["manual"],
    )
    fuchsia_component(
        name = "component_with_name_specified_named_component_D",
        manifest = ":generated_component_manifest_comnponent_C",
        component_name = "component_D",
    )
    fuchsia_package(
        name = "package_to_wrap_component_D",
        components = [
            ":component_with_name_specified_named_component_D",
        ],
        fuchsia_api_level = "HEAD",
    )
    _check_component_name_test(
        name = "check_component_name_taken_from_component_if_specified",
        target_under_test = ":package_to_wrap_component_D",
        component_name = "component_D",
    )

# Entry point from the BUILD file; macro for running each test case's macro and
# declaring a test suite that wraps them together.
def fuchsia_component_test_suite(name, **kwargs):
    _test_provider()
    _test_setting_component_names()

    native.test_suite(
        name = name,
        tests = [
            # _test_provider
            ":test_component_providers",
            ":test_driver_component_providers",

            # _test_setting_component_names
            ":check_component_name_taken_from_component_manifest_file_name",
            ":check_component_name_taken_from_component_manifest_component_name_attribute",
            ":check_component_name_taken_from_component_if_specified",
        ],
        **kwargs
    )
