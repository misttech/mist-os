# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""fuchsia_package() rule."""

load(
    ":providers.bzl",
    "FuchsiaComponentInfo",
    "FuchsiaDriverToolInfo",
    "FuchsiaPackageInfo",
    "FuchsiaPackageResourcesInfo",
)
load(":fuchsia_component.bzl", "fuchsia_component_for_unit_test")
load(":fuchsia_debug_symbols.bzl", "collect_debug_symbols", "get_build_id_dirs", "strip_resources")
load(":fuchsia_transition.bzl", "fuchsia_transition")
load(":package_publishing.bzl", "package_repo_path_from_label", "publish_package")
load(":utils.bzl", "label_name", "make_resource_struct", "rule_variants", "stub_executable")
load("//fuchsia/private/workflows:fuchsia_package_tasks.bzl", "fuchsia_package_tasks")

def fuchsia_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        components = [],
        resources = [],
        tools = [],
        **kwargs):
    """Builds a fuchsia package.

    This rule produces a fuchsia package which can be published to a package
    server and loaded on a device.

    The rule will return both package manifest json file which can be used later
    in the build system and an archive (.far) of the package which can be shared.

    This macro will expand out into several fuchsia tasks that can be run by a
    bazel invocation. Given a package definition, the following targets will be
    created.

    ```
    fuchsia_package(
        name = "pkg",
        components = [":my_component"],
        tools = [":my_tool"]
    )
    ```
    - pkg.help: Calling run on this target will show the valid macro-expanded targets
    - pkg.publish: Calling run on this target will publish the package
    - pkg.my_component: Calling run on this target will call `ffx component run`
        with the  component url if it is fuchsia_component instance and will
        call `ffx driver register` if it is a fuchsia_driver_component.
    - pkg.my_tool: Calling run on this target will call `ffx driver run-tool` if
        the tool is a fuchsia_driver_tool

    Args:
        name: The target name.
        components: A list of components to add to this package. The dependencies
          of these targets will have their debug symbols stripped and added to
          the build-id directory.
        resources: A list of additional resources to add to this package. These
          resources will not have debug symbols stripped.
        tools: Additional tools that should be added to this package.
        package_name: An optional name to use for this package, defaults to name.
        archive_name: An option name for the far file.
        **kwargs: extra attributes to pass along to the build rule.
    """

    # Temporary work around to pass in a repository name for driver URL.
    # This will help us unblock the Intel WIFI driver hot reload issue.
    driver_repository_name = kwargs.pop("driver_repository_name", None)

    _build_fuchsia_package(
        name = "%s_fuchsia_package" % name,
        components = components,
        resources = resources,
        tools = tools,
        package_name = package_name or name,
        archive_name = archive_name,
        **kwargs
    )

    fuchsia_package_tasks(
        name = name,
        package = "%s_fuchsia_package" % name,
        components = {component: component for component in components},
        tools = {tool: tool for tool in tools},
        driver_repository_name = driver_repository_name,
        **kwargs
    )

def _fuchsia_test_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        resources = [],
        _test_component_mapping,
        _components = [],
        **kwargs):
    """Defines test variants of fuchsia_package.

    See fuchsia_package for argument descriptions."""

    _build_fuchsia_package_test(
        name = "%s_fuchsia_package" % name,
        test_components = _test_component_mapping.values(),
        components = _components,
        resources = resources,
        package_name = package_name or name,
        archive_name = archive_name,
        **kwargs
    )

    fuchsia_package_tasks(
        name = name,
        package = "%s_fuchsia_package" % name,
        components = _test_component_mapping,
        is_test = True,
        **kwargs
    )

def fuchsia_test_package(
        *,
        name,
        test_components = [],
        components = [],
        **kwargs):
    """A test variant of fuchsia_package.

    See _fuchsia_test_package for additional arguments."""
    _fuchsia_test_package(
        name = name,
        _test_component_mapping = {component: component for component in test_components},
        _components = components,
        **kwargs
    )

def fuchsia_unittest_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        resources = [],
        unit_tests,
        **kwargs):
    # buildifier: disable=function-docstring-args
    """A variant of fuchsia_test_package containing unit tests.

    See _fuchsia_test_package for additional arguments."""

    test_component_mapping = {}
    for unit_test in unit_tests:
        test_component_mapping[unit_test] = "%s_unit_test" % label_name(unit_test)
        fuchsia_component_for_unit_test(
            name = test_component_mapping[unit_test],
            unit_test = unit_test,
            **kwargs
        )

    _fuchsia_test_package(
        name = name,
        package_name = package_name,
        archive_name = archive_name,
        resources = resources,
        _test_component_mapping = test_component_mapping,
        **kwargs
    )

def _build_fuchsia_package_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    archive_name = ctx.attr.archive_name or ctx.attr.package_name

    if not archive_name.endswith(".far"):
        archive_name += ".far"

    # where we will collect all of the temporary files
    pkg_dir = ctx.label.name + "_pkg/"

    # Declare all of the output files
    manifest = ctx.actions.declare_file(pkg_dir + "manifest")
    meta_package = ctx.actions.declare_file(pkg_dir + "meta/package")
    meta_far = ctx.actions.declare_file(pkg_dir + "meta.far")
    output_package_manifest = ctx.actions.declare_file(pkg_dir + "package_manifest.json")
    far_file = ctx.actions.declare_file(archive_name)

    # Environment variables that create an isolated FFX instance.
    ffx_isolate_build_dir = ctx.actions.declare_directory(pkg_dir + "_package_build.ffx")
    ffx_isolate_archive_dir = ctx.actions.declare_directory(pkg_dir + "_package_archive.ffx")

    # The Fuchsia target API level of this package
    api_level = sdk.default_api_level
    api_level_input = ["--api-level", str(api_level)]

    # All of the resources that will go into the package
    package_resources = [
        # Initially include the meta package
        make_resource_struct(
            src = meta_package,
            dest = "meta/package",
        ),
    ]

    # Resources that we will pass through the debug symbol stripping process
    resources_to_strip = []
    components = []
    drivers = []

    # Verify correctness of test vs non-test components.
    for test_component in ctx.attr.test_components:
        if not test_component[FuchsiaComponentInfo].is_test:
            fail("Please use `components` for non-test components.")
    for component in ctx.attr.components:
        if component[FuchsiaComponentInfo].is_test:
            fail("Please use `test_components` for test components.")

    # Collect all the resources from the deps
    for dep in ctx.attr.test_components + ctx.attr.components + ctx.attr.resources + ctx.attr.tools:
        if FuchsiaComponentInfo in dep:
            component_info = dep[FuchsiaComponentInfo]
            component_manifest = component_info.manifest
            component_dest = "meta/%s" % (component_manifest.basename)
            components.append(component_dest)

            if component_info.is_driver:
                drivers.append(component_dest)

            package_resources.append(
                # add the component manifest
                make_resource_struct(
                    src = component_manifest,
                    dest = component_dest,
                ),
            )
            resources_to_strip.extend([r for r in component_info.resources])
        elif FuchsiaDriverToolInfo in dep:
            resources_to_strip.extend(dep[FuchsiaDriverToolInfo].resources)
        elif FuchsiaPackageResourcesInfo in dep:
            # Don't strip debug symbols from resources.
            package_resources.extend(dep[FuchsiaPackageResourcesInfo].resources)
        else:
            fail("Unknown dependency type being added to package: %s" % dep.label)

    # Grab all of our stripped resources
    stripped_resources, _debug_info = strip_resources(ctx, resources_to_strip)
    package_resources.extend(stripped_resources)

    # Write our package_manifest file
    ctx.actions.write(
        output = manifest,
        content = "\n".join(["%s=%s" % (r.dest, r.src.path) for r in package_resources]),
    )

    # Create the meta/package file
    ctx.actions.run(
        executable = sdk.pm,
        arguments = [
            "-o",  # output directory
            manifest.dirname,
            "-n",  # name of the package
            ctx.attr.package_name,
            "init",
        ],
        outputs = [
            meta_package,
        ],
        mnemonic = "FuchsiaPmInit",
    )

    # The only input to the build step is the manifest but we need to
    # include all of the resources as inputs so that if they change the
    # package will get rebuilt.
    build_inputs = [r.src for r in package_resources] + [
        manifest,
        meta_package,
    ]

    repo_name_args = []
    if (ctx.attr.package_repository_name != None) and (ctx.attr.package_repository_name != ""):
        repo_name_args = ["--repository", ctx.attr.package_repository_name]

    # Build the package
    ctx.actions.run(
        executable = sdk.ffx_package,
        arguments = [
            "--isolate-dir",
            ffx_isolate_build_dir.path,
            "package",
            "build",
            manifest.path,
            "-o",  # output directory
            output_package_manifest.dirname,
            "--published-name",  # name of package
            ctx.attr.package_name,
        ] + api_level_input + repo_name_args,
        inputs = build_inputs,
        outputs = [
            output_package_manifest,
            meta_far,
            ffx_isolate_build_dir,
        ],
        mnemonic = "FuchsiaFfxPackageBuild",
        progress_message = "Building package for %s" % ctx.label,
    )

    artifact_inputs = [r.src for r in package_resources] + [
        output_package_manifest,
        meta_far,
    ]

    # Create the far file.
    ctx.actions.run(
        executable = sdk.ffx_package,
        arguments = [
            "--isolate-dir",
            ffx_isolate_archive_dir.path,
            "package",
            "archive",
            "create",
            output_package_manifest.path,
            "-o",
            far_file.path,
        ],
        inputs = artifact_inputs,
        outputs = [far_file, ffx_isolate_archive_dir],
        mnemonic = "FuchsiaFfxPackageArchiveCreate",
        progress_message = "Archiving package for %{label}",
    )

    output_files = [
        far_file,
        output_package_manifest,
        manifest,
        meta_far,
    ]

    # Attempt to publish if told to do so
    repo_path = package_repo_path_from_label(ctx.attr._package_repo_path)
    if repo_path:
        # TODO: collect all dependent packages
        stamp_file = publish_package(ctx, sdk.pm, repo_path, [output_package_manifest])
        output_files.append(stamp_file)

    # Sanity check that we are not trying to put 2 different resources at the same mountpoint
    collected_blobs = {}
    for resource in package_resources:
        if resource.dest in collected_blobs and resource.src.path != collected_blobs[resource.dest]:
            fail("Trying to add multiple resources with the same filename and different content")
        else:
            collected_blobs[resource.dest] = resource.src.path

    return [
        DefaultInfo(files = depset(output_files), executable = stub_executable(ctx)),
        FuchsiaPackageInfo(
            far_file = far_file,
            package_manifest = output_package_manifest,
            files = [output_package_manifest, meta_far] + build_inputs,
            package_name = ctx.attr.package_name,
            components = components,
            drivers = drivers,
            meta_far = meta_far,
            package_resources = package_resources,

            # TODO: Remove this field, change usages to FuchsiaDebugSymbolInfo.
            build_id_dir = get_build_id_dirs(_debug_info)[0],
        ),
        collect_debug_symbols(
            _debug_info,
            ctx.attr.test_components,
            ctx.attr.components,
            ctx.attr.resources,
            ctx.attr.tools,
            ctx.attr._fuchsia_sdk_debug_symbols,
        ),
    ]

_build_fuchsia_package, _build_fuchsia_package_test = rule_variants(
    variants = (None, "test"),
    doc = "Builds a fuchsia package.",
    implementation = _build_fuchsia_package_impl,
    cfg = fuchsia_transition,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain", "@bazel_tools//tools/cpp:toolchain_type"],
    attrs = {
        "package_name": attr.string(
            doc = "The name of the package",
            mandatory = True,
        ),
        "archive_name": attr.string(
            doc = "What to name the archive. The .far file will be appended if not in this name. Defaults to package_name",
        ),
        # TODO(https://fxbug.dev/114334): Improve doc for this field when we
        # have more clarity from the bug.
        "package_repository_name": attr.string(
            doc = "Repository name of this package, defaults to None",
        ),
        "components": attr.label_list(
            doc = "The list of components included in this package",
            providers = [FuchsiaComponentInfo],
        ),
        "test_components": attr.label_list(
            doc = "The list of test components included in this package",
            providers = [FuchsiaComponentInfo],
        ),
        "resources": attr.label_list(
            doc = "The list of resources included in this package",
            providers = [FuchsiaPackageResourcesInfo],
        ),
        "tools": attr.label_list(
            doc = "The list of tools included in this package",
            providers = [FuchsiaDriverToolInfo],
        ),
        "_fuchsia_sdk_debug_symbols": attr.label(
            doc = "Include debug symbols from @fuchsia_sdk.",
            default = "@fuchsia_sdk//:debug_symbols",
        ),
        "_package_repo_path": attr.label(
            doc = "The command line flag used to publish packages.",
            default = "//fuchsia:package_repo",
        ),
        "_elf_strip_tool": attr.label(
            default = "//fuchsia/tools:elf_strip",
            executable = True,
            cfg = "exec",
        ),
        "_generate_symbols_dir_tool": attr.label(
            default = "//fuchsia/tools:generate_symbols_dir",
            executable = True,
            cfg = "exec",
        ),
        "_cc_toolchain": attr.label(
            default = Label("@bazel_tools//tools/cpp:current_cc_toolchain"),
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)
