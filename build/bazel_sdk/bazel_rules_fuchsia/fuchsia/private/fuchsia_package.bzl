# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""fuchsia_package() rule."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")
load("//fuchsia/private/workflows:fuchsia_package_tasks.bzl", "fuchsia_package_tasks")
load(":fuchsia_api_level.bzl", "FUCHSIA_API_LEVEL_ATTRS", "get_fuchsia_api_level")
load(
    ":fuchsia_debug_symbols.bzl",
    "FUCHSIA_DEBUG_SYMBOLS_ATTRS",
    "collect_debug_symbols",
    "find_and_process_unstripped_binaries",
    "strip_resources",
)
load(":fuchsia_package_resource.bzl", "fuchsia_find_all_package_resources")
load(":fuchsia_transition.bzl", "fuchsia_transition")
load(
    ":providers.bzl",
    "FuchsiaCollectedPackageResourcesInfo",
    "FuchsiaComponentInfo",
    "FuchsiaDebugSymbolInfo",
    "FuchsiaDriverToolInfo",
    "FuchsiaPackageInfo",
    "FuchsiaPackageResourcesInfo",
    "FuchsiaPackagedComponentInfo",
    "FuchsiaStructuredConfigInfo",
)
load(
    ":utils.bzl",
    "append_suffix_to_label",
    "fuchsia_cpu_from_ctx",
    "label_name",
    "make_resource_struct",
    "rule_variants",
    "stub_executable",
)

def get_driver_component_manifests(package):
    """Returns a list of the manifest paths for drivers in the package

    Args:
        package: the package to parse
    """
    return [entry.dest for entry in package[FuchsiaPackageInfo].packaged_components if entry.component_info.is_driver]

def get_component_manifests(package):
    """Returns a list of the manifest paths for all components in the package

    Args:
        package: the package to parse.
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
        tags = [],
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
        tags: Forward additional tags to all generated targets.
        **kwargs: extra attributes to pass along to the build rule.
    """

    # This is only used when we want to disable a pre-existing driver so we can
    # register another driver.
    disable_repository_name = kwargs.pop("disable_repository_name", None)

    package_repository_name = kwargs.pop("package_repository_name", None)

    _deps_to_search = components + resources + tools

    processed_binaries = "%s_fuchsia_package.elf_binaries" % name
    find_and_process_unstripped_binaries(
        name = processed_binaries,
        deps = _deps_to_search,
        tags = tags + ["manual"],
        **kwargs
    )

    collected_resources = "%s_fuchsia_package.resources" % name
    fuchsia_find_all_package_resources(
        name = collected_resources,
        deps = _deps_to_search,
        tags = tags + ["manual"],
        **kwargs
    )

    _build_fuchsia_package(
        name = "%s_fuchsia_package" % name,
        components = components,
        resources = resources,
        processed_binaries = processed_binaries,
        collected_resources = collected_resources,
        tools = tools,
        subpackages = subpackages,
        subpackages_to_flatten = subpackages_to_flatten,
        package_name = package_name or name,
        archive_name = archive_name,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        package_repository_name = package_repository_name,
        tags = tags + ["manual"],
        **kwargs
    )

    fuchsia_package_tasks(
        name = name,
        package = "%s_fuchsia_package" % name,
        component_run_tags = [label_name(c) for c in components],
        tools = {tool: tool for tool in tools},
        package_repository_name = package_repository_name,
        disable_repository_name = disable_repository_name,
        # TODO(b/339099331) fuchsia_packages that are testonly shouldn't have the
        # full set of tasks.
        is_test = kwargs.get("testonly", False),
        tags = tags,
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
        target_compatible_with = [],
        **kwargs):
    """Defines test variants of fuchsia_package.

    See fuchsia_package for argument descriptions."""

    _deps_to_search = _components + resources + _test_component_mapping.values()

    processed_binaries = "%s_fuchsia_package.elf_binaries" % name
    find_and_process_unstripped_binaries(
        name = processed_binaries,
        deps = _deps_to_search,
        testonly = True,
        tags = tags + ["manual"],
        target_compatible_with = target_compatible_with,
    )

    collected_resources = "%s_fuchsia_package.resources" % name
    fuchsia_find_all_package_resources(
        name = collected_resources,
        deps = _deps_to_search,
        testonly = True,
        tags = tags + ["manual"],
        target_compatible_with = target_compatible_with,
    )

    _build_fuchsia_package_test(
        name = "%s_fuchsia_package" % name,
        test_components = _test_component_mapping.values(),
        components = _components,
        resources = resources,
        processed_binaries = processed_binaries,
        collected_resources = collected_resources,
        subpackages = subpackages,
        subpackages_to_flatten = subpackages_to_flatten,
        package_name = package_name or name,
        archive_name = archive_name,
        fuchsia_api_level = fuchsia_api_level,
        platform = platform,
        target_compatible_with = target_compatible_with,
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
        target_compatible_with = target_compatible_with,
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
    """A test variant of fuchsia_test_package

    See _fuchsia_test_package for additional arguments.


"""
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
        unit_tests,
        **kwargs):
    """A wrapper around fuchsia_test_package which doesn't require components.

    This rule allows users to construct a fuchsia_test_package without having to
    create components for each test. This allows users to take a dependency on a
    rule like fuchsia_cc_test directly.

    It is up to the author of the rule being depended on to craft the rule in a
    way that it exposes the generated test component to this rule. The convention
    is that a unit_test target must also create a fuchsia_test_component which
    has the name <name>.unittest_component.

    See fuchsia_test_package for additional arguments.

    Args:
        name: This target name.
        unit_tests: The unit_test targets. These targets must have a generated
          fuchsia_test_component with the name <name>.unittest_component.
        **kwargs: Arguments to forward to the fuchsia_test_package.
    """

    fuchsia_test_package(
        name = name,
        test_components = [append_suffix_to_label(t, "unittest_component") for t in unit_tests],
        **kwargs
    )

def _build_fuchsia_package_impl(ctx):
    sdk = get_fuchsia_sdk_toolchain(ctx)
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

    # Add all of the collected resources
    package_resources.extend(
        ctx.attr.collected_resources[FuchsiaCollectedPackageResourcesInfo].collected_resources.to_list(),
    )

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
    # TODO(342560609) Move all resource publishing from components into the
    # component rules so they get collected into the collected_resources attr.
    for dep in ctx.attr.test_components + ctx.attr.components:
        if FuchsiaStructuredConfigInfo in dep:
            sc_info = dep[FuchsiaStructuredConfigInfo]
            package_resources.append(
                # add the CVF file
                make_resource_struct(
                    src = sc_info.cvf_source,
                    dest = sc_info.cvf_dest,
                ),
            )

        if FuchsiaComponentInfo in dep:
            component_info = dep[FuchsiaComponentInfo]
            component_dest = "meta/%s.cm" % (component_info.name)

            packaged_components.append(FuchsiaPackagedComponentInfo(
                component_info = component_info,
                dest = component_dest,
            ))
        else:
            fail("Unknown dependency type being added to package: %s" % dep.label)

    # This build-id directory is used for the in-tree build
    # LINT.IfChange
    build_id_path = ctx.label.name + "_build_id_dir"
    # LINT.ThenChange(//build/bazel/bazel_fuchsia_package.gni)

    # Grab all of our stripped resources
    stripped_resources, _debug_info = strip_resources(ctx, resources_to_strip, build_id_path = build_id_path)
    package_resources.extend(stripped_resources)

    # Add the resources for stripped ELF binaries.
    package_resources.extend(ctx.attr.processed_binaries[FuchsiaPackageResourcesInfo].resources)

    # Write our package_manifest file. If we have subpackage to flatten, we will
    # parse the subpackages contents, and append them into package manifest of
    # parent package.  Sort for determinism.
    content = "\n".join(["%s=%s" % (r.dest, r.src.path) for r in sorted(package_resources, key = lambda s: s.dest)])

    meta_content_inputs = []
    if ctx.attr.subpackages_to_flatten:
        subpackage_manifests = []
        for package in ctx.attr.subpackages_to_flatten:
            meta_content_inputs.extend(package[FuchsiaPackageInfo].files)
            subpackage_manifests.append(package[FuchsiaPackageInfo].package_manifest.path)

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
            ] + subpackage_manifests,
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

    # Validate binary paths in cmls
    component_manifest_files = [c.component_info.manifest for c in packaged_components]
    depfile = ctx.actions.declare_file(pkg_dir + "components_validation.depfile")
    ctx.actions.run(
        executable = ctx.executable._validate_component_manifests,
        arguments = [
            "--cmc",
            sdk.cmc.path,
            "--component-manifest-paths",
            ",".join([c.path for c in component_manifest_files]),
            "--package-manifest",
            manifest.path,
            "--output",
            depfile.path,
        ],
        inputs = [sdk.cmc, manifest] + component_manifest_files,
        outputs = [depfile],
        mnemonic = "CmcValidate",
        progress_message = "Validating binary paths in cml for %s" % ctx.label,
    )

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
        inputs = build_inputs + subpackages_inputs + meta_content_inputs + [depfile],
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
            fail("Trying to add multiple resources with the same filename and different content", resource)
        else:
            collected_blobs[resource.dest] = resource.src.path

    # A FuchsiaDebugSymbolInfo value that covers all debug symbol
    # directories needed for this package.
    #
    # TODO(https://fxbug.dev/339038603): Only use processed_binaries
    # for this once all dependencies use the right rules to expose
    # their debug symbols.
    #
    fuchsia_debug_symbols_info = collect_debug_symbols(
        _debug_info,
        ctx.attr.subpackages,
        ctx.attr.test_components,
        ctx.attr.components,
        ctx.attr.resources,
        ctx.attr.processed_binaries,
        ctx.attr.tools,
        ctx.attr._fuchsia_sdk_debug_symbols,
    )

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
            build_id_dirs = fuchsia_debug_symbols_info.build_id_dirs.values(),
        ),
        fuchsia_debug_symbols_info,
        OutputGroupInfo(
            build_id_dirs = depset(transitive = fuchsia_debug_symbols_info.build_id_dirs.values()),
        ),
    ]

_build_fuchsia_package, _build_fuchsia_package_test = rule_variants(
    variants = (None, "test"),
    doc = "Builds a fuchsia package.",
    implementation = _build_fuchsia_package_impl,
    cfg = fuchsia_transition,
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION + ["@bazel_tools//tools/cpp:toolchain_type"],
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
        "processed_binaries": attr.label(
            doc = "Label to a find_and_process_unstripped_binaries() target for this package.",
            providers = [FuchsiaPackageResourcesInfo, FuchsiaDebugSymbolInfo],
        ),
        "collected_resources": attr.label(
            doc = "Label to a fuchsia_find_all_package_resources() target for this package.",
            providers = [FuchsiaCollectedPackageResourcesInfo],
            mandatory = True,
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
        "hack_ignore_cpp": attr.bool(
            doc = "This value is no longer used and will be removed shortly.",
            default = False,
        ),
        "_fuchsia_sdk_debug_symbols": attr.label(
            doc = "Include debug symbols from @fuchsia_sdk.",
            default = "@fuchsia_sdk//:debug_symbols",
        ),
        "_meta_content_append_tool": attr.label(
            default = "//fuchsia/tools:meta_content_append",
            executable = True,
            cfg = "exec",
        ),
        "_validate_component_manifests": attr.label(
            default = "//fuchsia/tools:validate_component_manifests",
            executable = True,
            cfg = "exec",
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    } | COMPATIBILITY.FUCHSIA_ATTRS | FUCHSIA_API_LEVEL_ATTRS | FUCHSIA_DEBUG_SYMBOLS_ATTRS,
)
