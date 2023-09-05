# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":fuchsia_shell_task.bzl", "shell_task_rule")
load(":fuchsia_task_ffx.bzl", "fuchsia_task_ffx")
load(":fuchsia_task_publish.bzl", "fuchsia_task_publish")
load(":fuchsia_task_register_debug_symbols.bzl", "fuchsia_task_register_debug_symbols")
load(":fuchsia_task_run_component.bzl", "fuchsia_task_run_component")
load(":fuchsia_task_run_driver_tool.bzl", "fuchsia_task_run_driver_tool")
load(":fuchsia_task_verbs.bzl", "make_help_executable", "verbs")
load(":fuchsia_workflow.bzl", "fuchsia_workflow", "fuchsia_workflow_rule")
load(":providers.bzl", "FuchsiaDebugSymbolInfo", "FuchsiaPackageInfo", "FuchsiaWorkflowInfo")
load(":utils.bzl", "flatten", "label_name", "normalized_target_name")

def _to_verb(label):
    return verbs.custom(label_name(label))

def _fuchsia_package_help_impl(ctx, make_shell_task):
    help = make_help_executable(ctx, dict((
        [(verbs.noverb, "Run all test components within this test package.")] if ctx.attr.is_test and ctx.attr.components else []
    ) + [
        (verbs.help, "Print this help message."),
        (verbs.debug_symbols, "Register this package's debug symbols."),
        (verbs.publish, "Publish this package and register debug symbols."),
    ] + [
        (_to_verb(component), "Publish this package and run '%s' with debug symbols." % component)
        for component in ctx.attr.components
    ] + [
        (_to_verb(tool), "Publish this package and run '%s' with debug symbols" % tool)
        for tool in ctx.attr.tools
    ]), name = ctx.attr.top_level_name)
    return make_shell_task([help])

# buildifier: disable=unused-variable
(
    __fuchsia_package_help,
    _fuchsia_package_help_for_test,
    _fuchsia_package_help,
) = shell_task_rule(
    implementation = _fuchsia_package_help_impl,
    doc = "Prints valid runnable sub-targets in a package.",
    attrs = {
        "is_test": attr.bool(
            doc = "Whether the package is a test package.",
            mandatory = True,
        ),
        "package": attr.label(
            doc = "The package.",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "components": attr.string_list(
            doc = "The component names.",
            mandatory = True,
        ),
        "tools": attr.string_list(
            doc = "The driver tool names.",
            mandatory = True,
        ),
        "debug_symbols_task": attr.label(
            doc = "The debug symbols task associated with the package.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "publish_task": attr.label(
            doc = "The package publishing task associated with the package.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "top_level_name": attr.string(
            doc = "The top level target name associated with these tasks",
            mandatory = True,
        ),
    },
)

def _fuchsia_package_default_task_impl(ctx, make_workflow):
    default_workflow = make_workflow(sequence = flatten([
        ctx.attr.debug_symbols_task,
        ctx.attr.publish_task,
    ] + ctx.attr.component_run_tasks + [
        ctx.attr.publish_cleanup_task or [],
    ]) if (
        ctx.attr.is_test and ctx.attr.component_run_tasks
    ) else [ctx.attr.help_task])
    return [
        DefaultInfo(
            files = depset(transitive = [provider.files, ctx.attr.package[DefaultInfo].files]),
            runfiles = provider.default_runfiles,
            executable = provider.files.to_list()[0],
        ) if type(provider) == "DefaultInfo" else provider
        for provider in default_workflow
    ] + [
        ctx.attr.package[FuchsiaPackageInfo],
        ctx.attr.package[FuchsiaDebugSymbolInfo],
        # Expose the generated far file and debug symbols.
        # This is also used in fuchsia.git, see https://fxbug.dev/115699 and
        # https://fxbug.dev/119049.
        OutputGroupInfo(
            far_file = depset([ctx.attr.package[FuchsiaPackageInfo].far_file]),
            build_id_dirs = depset(transitive = ctx.attr.package[FuchsiaDebugSymbolInfo].build_id_dirs.values()),
        ),
    ]

# buildifier: disable=unused-variable
(
    __fuchsia_package_default_task,
    _fuchsia_package_default_task_for_test,
    _fuchsia_package_default_task,
) = fuchsia_workflow_rule(
    implementation = _fuchsia_package_default_task_impl,
    doc = "Runs all test components for test packages, or prints a help message.",
    attrs = {
        "is_test": attr.bool(
            doc = "Whether the package is a test package.",
            mandatory = True,
        ),
        "help_task": attr.label(
            doc = "The help task describing valid package subtargets.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "debug_symbols_task": attr.label(
            doc = "The debug symbols task associated with the package.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "publish_task": attr.label(
            doc = "The package publishing task associated with the package.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "publish_cleanup_task": attr.label(
            doc = "The package publishing cleanup task associated with the package.",
            providers = [FuchsiaWorkflowInfo],
        ),
        "component_run_tasks": attr.label_list(
            doc = "The component run tasks.",
            providers = [FuchsiaWorkflowInfo],
            mandatory = True,
        ),
        "package": attr.label(
            doc = "The package.",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
    },
)

# buildifier: disable=function-docstring
def fuchsia_package_tasks(
        *,
        name,
        package,
        components,
        tools = {},
        is_test = False,
        tags = [],
        package_repository_name = None,
        disable_repository_name = None,
        **kwargs):
    # TODO(fxbug.dev/98996): Use ffx isolation. ffx test run currently needs
    # to access ~/.local/share/Fuchsia/ffx/ or else it crashes.
    top_level_tags = tags + (["no-sandbox", "no-cache"] if is_test else [])

    # Mark test children as manual.
    manual_test = ["manual"] if is_test else []

    # Override testonly since it's used to determine test vs non-test rule
    # variant selection for workflows.
    kwargs["testonly"] = is_test

    # For `bazel run :pkg.debug_symbols`.
    debug_symbols_task = verbs.debug_symbols(name)
    fuchsia_task_register_debug_symbols(
        name = debug_symbols_task,
        deps = [package],
        apply_fuchsia_transition = True,
        tags = top_level_tags,
        **kwargs
    )

    # For `bazel run :pkg.publish`.
    publish_task = verbs.publish(name)
    anonymous_publish_task = "%s_anonymous" % publish_task
    anonymous_repo_name = "bazel.%s" % normalized_target_name(anonymous_publish_task)
    fuchsia_task_publish(
        name = anonymous_publish_task,
        packages = [package],
        package_repository_name = package_repository_name or anonymous_repo_name,
        **kwargs
    )
    fuchsia_task_ffx(
        name = verbs.delete_repo(anonymous_publish_task),
        arguments = [
            "repository",
            "remove",
            anonymous_repo_name,
        ],
        **kwargs
    )
    publish_only_task = "%s_only" % publish_task
    fuchsia_task_publish(
        name = publish_only_task,
        packages = [package],
        package_repository_name = package_repository_name,
        **kwargs
    )
    fuchsia_workflow(
        name = publish_task,
        sequence = [
            debug_symbols_task,
            publish_only_task,
        ],
        apply_fuchsia_transition = True,
        tags = top_level_tags,
        **kwargs
    )

    # For `bazel run :pkg.help`.
    help_task = verbs.help(name)
    _fuchsia_package_help(
        name = help_task,
        package = package,
        components = components.keys(),
        tools = tools,
        debug_symbols_task = debug_symbols_task,
        publish_task = publish_task,
        top_level_name = name,
        is_test = is_test,
        apply_fuchsia_transition = True,
        tags = top_level_tags,
        **kwargs
    )

    # For `bazel run :pkg.component`.
    component_run_tasks = []
    for label, component in components.items():
        component_run_task = _to_verb(label)(name)
        component_run_tasks.append("%s.run_only" % component_run_task)
        fuchsia_task_run_component(
            name = component_run_tasks[-1],
            default_argument_scope = "global",
            repository = package_repository_name or anonymous_repo_name,
            package = package,
            component = component,
            tags = tags,
            disable_repository = disable_repository_name,
            **kwargs
        )

        fuchsia_workflow(
            name = component_run_task,
            sequence = [
                debug_symbols_task,
                anonymous_publish_task,
                component_run_tasks[-1],
            ] + ([] if package_repository_name else [
                verbs.delete_repo(anonymous_publish_task),
            ]),
            apply_fuchsia_transition = True,
            tags = top_level_tags + manual_test,
            **kwargs
        )

    # For `bazel run :pkg.tool`.
    for label, tool in tools.items():
        tool_run_task = _to_verb(label)(name)
        fuchsia_task_run_driver_tool(
            name = "%s.run_only" % tool_run_task,
            default_argument_scope = "global",
            repository = package_repository_name or anonymous_repo_name,
            package = package,
            tool = tool,
            tags = tags,
            **kwargs
        )

        fuchsia_workflow(
            name = tool_run_task,
            sequence = [
                debug_symbols_task,
                anonymous_publish_task,
                "%s.run_only" % tool_run_task,
            ] + ([] if package_repository_name else [
                verbs.delete_repo(anonymous_publish_task),
            ]),
            apply_fuchsia_transition = True,
            tags = top_level_tags,
            **kwargs
        )

    # For `bazel run :pkg`.
    _fuchsia_package_default_task(
        name = name,
        help_task = help_task,
        debug_symbols_task = debug_symbols_task,
        publish_task = anonymous_publish_task,
        publish_cleanup_task = None if (
            package_repository_name
        ) else verbs.delete_repo(anonymous_publish_task),
        component_run_tasks = component_run_tasks,
        is_test = is_test,
        package = package,
        apply_fuchsia_transition = True,
        tags = top_level_tags,
        **kwargs
    )
