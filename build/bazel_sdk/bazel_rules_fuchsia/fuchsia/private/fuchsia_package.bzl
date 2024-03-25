# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""fuchsia_package() rule."""

load("//fuchsia/private/workflows:fuchsia_package_tasks.bzl", "fuchsia_package_tasks")
load(":fuchsia_api_level.bzl", "FUCHSIA_API_LEVEL_ATTRS", "get_fuchsia_api_level")
load(":fuchsia_component.bzl", "fuchsia_component_for_unit_test")
load(":fuchsia_debug_symbols.bzl", "collect_debug_symbols", "get_build_id_dirs", "strip_resources")
load(":fuchsia_transition.bzl", "fuchsia_transition")
load(
    ":providers.bzl",
    "FuchsiaComponentInfo",
    "FuchsiaDriverToolInfo",
    "FuchsiaPackageInfo",
    "FuchsiaPackageResourcesInfo",
    "FuchsiaPackagedComponentInfo",
)
load(":utils.bzl", "fuchsia_cpu_from_ctx", "label_name", "make_resource_struct", "rule_variants", "stub_executable")

_FUCHSIA_OS_PLATFORM = "@platforms//os:fuchsia"

def get_driver_component_manifests(package):
    """ Returns a list of the manifest paths for drivers in the package

    Args:
        - package: the package to parse
    """
    return [entry.dest for entry in package[FuchsiaPackageInfo].packaged_components if entry.component_info.is_driver]

def get_component_manifests(package):
    """ Returns a list of the manifest paths for all components in the package

    Args:
        - package: the package to parse
    """
    return [entry.dest for entry in package[FuchsiaPackageInfo].packaged_components]

def fuchsia_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        platform = None,
        fuchsia_api_level = None,
        components = [],
        resources = [],
        tools = [],
        subpackages = [],
        subpackages_to_flatten = [],
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
        subpackages: Additional subpackages that should be added to this package.
        subpackages_to_flatten: The list of subpackages included in this package.
          The packages included in this list will be cracked open and all the
          components included will be include in the parent package.
        package_name: An optional name to use for this package, defaults to name.
        archive_name: An option name for the far file.
        fuchsia_api_level: The API level to build for.
        platform: Optionally override the platform to build the package for.
        **kwargs: extra attributes to pass along to the build rule.
    """

    # This is only used when we want to disable a pre-existing driver so we can
    # register another driver.
    disable_repository_name = kwargs.pop("disable_repository_name", None)

    # Fuchsia packages are only compatible with the fuchsia OS.
    target_compat = kwargs.pop("target_compatible_with", [])
    if _FUCHSIA_OS_PLATFORM not in target_compat:
        target_compat.append(_FUCHSIA_OS_PLATFORM)

    _build_fuchsia_package(
        name = "%s_fuchsia_package" % name,
        components = components,
        resources = resources,
        tools = tools,
        subpackages = subpackages,
        subpackages_to_flatten = subpackages_to_flatten,
        package_name = package_name or name,
        archive_name = archive_name,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        target_compatible_with = target_compat,
        **kwargs
    )

    fuchsia_package_tasks(
        name = name,
        package = "%s_fuchsia_package" % name,
        component_run_tags = [label_name(c) for c in components],
        tools = {tool: tool for tool in tools},
        disable_repository_name = disable_repository_name,
        **kwargs
    )

def _fuchsia_test_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        resources = [],
        fuchsia_api_level = None,
        platform = None,
        _test_component_mapping,
        _components = [],
        subpackages = [],
        subpackages_to_flatten = [],
        test_realm = None,
        tags = [],
        **kwargs):
    """Defines test variants of fuchsia_package.

    See fuchsia_package for argument descriptions."""

    # Fuchsia packages are only compatible with the fuchsia OS.
    target_compat = kwargs.pop("target_compatible_with", [])
    if _FUCHSIA_OS_PLATFORM not in target_compat:
        target_compat.append(_FUCHSIA_OS_PLATFORM)

    _build_fuchsia_package_test(
        name = "%s_fuchsia_package" % name,
        test_components = _test_component_mapping.values(),
        components = _components,
        resources = resources,
        subpackages = subpackages,
        subpackages_to_flatten = subpackages_to_flatten,
        package_name = package_name or name,
        archive_name = archive_name,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        target_compatible_with = target_compat,
        tags = tags + ["manual"],
        **kwargs
    )

    fuchsia_package_tasks(
        name = name,
        package = "%s_fuchsia_package" % name,
        component_run_tags = _test_component_mapping.keys(),
        is_test = True,
        test_realm = test_realm,
        tags = tags,
        **kwargs
    )

def fuchsia_test_package(
        *,
        name,
        test_components = [],
        components = [],
        subpackages_to_flatten = [],
        fuchsia_api_level = None,
        platform = None,
        **kwargs):
    """A test variant of fuchsia_package.

    See _fuchsia_test_package for additional arguments."""
    _fuchsia_test_package(
        name = name,
        _test_component_mapping = {label_name(component): component for component in test_components},
        _components = components,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        subpackages_to_flatten = subpackages_to_flatten,
        **kwargs
    )

def fuchsia_unittest_package(
        *,
        name,
        package_name = None,
        archive_name = None,
        fuchsia_api_level = None,
        platform = None,
        resources = [],
        subpackages = [],
        subpackages_to_flatten = [],
        unit_tests,
        tags = [],
        **kwargs):
    # buildifier: disable=function-docstring-args
    """A variant of fuchsia_test_package containing unit tests.

    See _fuchsia_test_package for additional arguments."""
    test_component_mapping = {}
    for unit_test in unit_tests:
        run_tag = label_name(unit_test)
        test_component_mapping[run_tag] = "%s.%s.autogen_component" % (name, run_tag)

        fuchsia_component_for_unit_test(
            name = test_component_mapping[run_tag],
            unit_test = unit_test,
            run_tag = run_tag,
            tags = tags + ["manual"],
            **kwargs
        )

    _fuchsia_test_package(
        name = name,
        package_name = package_name,
        archive_name = archive_name,
        resources = resources,
        subpackages = subpackages,
        subpackages_to_flatten = subpackages_to_flatten,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        _test_component_mapping = test_component_mapping,
        tags = tags,
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
    api_level_input = ["--api-level", get_fuchsia_api_level(ctx)]

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
    packaged_components = []

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

            packaged_components.append(FuchsiaPackagedComponentInfo(
                component_info = component_info,
                dest = component_dest,
            ))

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

    # This build-id directory is used for the in-tree build
    # LINT.IfChange
    build_id_path = ctx.label.name + "_build_id_dir"
    # LINT.ThenChange(//build/bazel/bazel_fuchsia_package.gni)

    # Grab all of our stripped resources
    stripped_resources, _debug_info = strip_resources(ctx, resources_to_strip, build_id_path = build_id_path)
    package_resources.extend(stripped_resources)

    # Write our package_manifest file. If we have subpackage to flatten, we will
    # parse the subpackages contents, and append them into package manifest of
    # parent package.
    content = "\n".join(["%s=%s" % (r.dest, r.src.path) for r in package_resources])
    meta_content_inputs = []
    if ctx.attr.subpackages_to_flatten:
        subpackage_manfiests = []
        for package in ctx.attr.subpackages_to_flatten:
            meta_content_inputs.extend(package[FuchsiaPackageInfo].files)
            subpackage_manfiests.append(package[FuchsiaPackageInfo].package_manifest.path)

        meta_contents_dir = ctx.actions.declare_directory(pkg_dir + "_meta_contents_dir")
        ffx_meta_extract_dir = ctx.actions.declare_directory(pkg_dir + "_extract_archive.ffx")

        ctx.actions.run(
            executable = ctx.executable._meta_content_append_tool,
            arguments = [
                "--ffx",
                sdk.ffx_package.path,
                "--ffx-isolate-dir",
                ffx_meta_extract_dir.path,
                "--manifest-path",
                manifest.path,
                "--original-content",
                content,
                "--meta-contents-dir",
                meta_contents_dir.path,
                "--subpackage-manifests",
            ] + subpackage_manfiests,
            inputs = meta_content_inputs + [sdk.ffx_package],
            outputs = [
                manifest,
                meta_contents_dir,
                ffx_meta_extract_dir,
            ],
            progress_message = "Building manifest for %s" % ctx.label,
        )
        meta_content_inputs.append(meta_contents_dir)

    else:
        ctx.actions.write(
            output = manifest,
            content = content,
        )

    # Create the meta/package file
    ctx.actions.write(
        meta_package,
        content = json.encode_indent({
            "name": ctx.attr.package_name,
            "version": "0",
        }),
    )

    # The only input to the build step is the manifest but we need to
    # include all of the resources as inputs so that if they change the
    # package will get rebuilt.
    build_inputs = [r.src for r in package_resources] + [
        manifest,
        meta_package,
    ]

    repo_name_args = []
    if ctx.attr.package_repository_name:
        repo_name_args = ["--repository", ctx.attr.package_repository_name]

    subpackages_args = []
    subpackages_inputs = []
    subpackages = ctx.attr.subpackages
    if subpackages:
        # Create the subpackages file
        subpackages_json = ctx.actions.declare_file(pkg_dir + "/subpackages.json")
        ctx.actions.write(
            subpackages_json,
            content = json.encode_indent([{
                "package_manifest_file": subpackage[FuchsiaPackageInfo].package_manifest.path,
            } for subpackage in subpackages]),
        )

        subpackages_args = ["--subpackages-build-manifest-path", subpackages_json.path]
        subpackages_inputs = [subpackages_json] + [
            file
            for subpackage in subpackages
            for file in subpackage[FuchsiaPackageInfo].files
        ]

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
        ] + subpackages_args + api_level_input + repo_name_args,
        inputs = build_inputs + subpackages_inputs + meta_content_inputs,
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
    ] + subpackages_inputs + meta_content_inputs

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
    ] + build_inputs

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
            fuchsia_cpu = fuchsia_cpu_from_ctx(ctx),
            far_file = far_file,
            package_manifest = output_package_manifest,
            files = [output_package_manifest, meta_far] + build_inputs,
            package_name = ctx.attr.package_name,
            meta_far = meta_far,
            package_resources = package_resources,
            packaged_components = packaged_components,
            # TODO: Remove this field, change usages to FuchsiaDebugSymbolInfo.
            build_id_dir = (get_build_id_dirs(_debug_info) + [None])[0],
        ),
        collect_debug_symbols(
            _debug_info,
            ctx.attr.subpackages,
            ctx.attr.test_components,
            ctx.attr.components,
            ctx.attr.resources,
            ctx.attr.tools,
            ctx.attr._fuchsia_sdk_debug_symbols,
        ),
        OutputGroupInfo(
            build_id_dir = _debug_info.build_id_dirs.values()[0],
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
        # TODO(https://fxbug.dev/42065627): Improve doc for this field when we
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
        "subpackages": attr.label_list(
            doc = "The list of subpackages included in this package",
            providers = [FuchsiaPackageInfo],
        ),
        "subpackages_to_flatten": attr.label_list(
            doc = """The list of subpackages included in this package.

            The packages included in this list will be cracked open and all the
            components included will be include in the parent package.

            This is a workaround for lack of support for subpackages in
            driver_test_realm. Please don't use it without consulting with the
            SDK Experiences team!

            TODO(https://fxbug.dev/330189874): Remove this attribute.
            """,
            providers = [FuchsiaPackageInfo],
        ),
        "fuchsia_api_level": attr.string(
            doc = """The Fuchsia API level to use when building this package.

            This value will be sent to the fidl compiler and cc_* rules when
            compiling dependencies.
            """,
        ),
        "platform": attr.string(
            doc = """The Fuchsia platform to build for.

            If this value is not set we will fall back to the cpu setting to determine
            the correct platform.
            """,
        ),
        "_fuchsia_sdk_debug_symbols": attr.label(
            doc = "Include debug symbols from @fuchsia_sdk.",
            default = "@fuchsia_sdk//:debug_symbols",
        ),
        "_elf_strip_tool": attr.label(
            default = "//fuchsia/tools:elf_strip",
            executable = True,
            cfg = "exec",
        ),
        "_meta_content_append_tool": attr.label(
            default = "//fuchsia/tools:meta_content_append",
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
    } | FUCHSIA_API_LEVEL_ATTRS,
)
