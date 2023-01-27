# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":fuchsia_task_ffx.bzl", "fuchsia_task_ffx")
load(
    "//fuchsia/private/workflows:fuchsia_workflow.bzl",
    "fuchsia_workflow",
)
load(
    "@rules_fuchsia//fuchsia/private/workflows:fuchsia_task_autodetect_target.bzl",
    "fuchsia_task_autodetect_target",
)
load(
    "@rules_fuchsia//fuchsia/private/workflows:fuchsia_package_repository_tasks.bzl",
    "fuchsia_task_repository_register_with_default_target",
)
load(
    "//fuchsia/private/workflows:fuchsia_emulator_tasks.bzl",
    "fuchsia_task_emulator_register_repository",
)
load("//fuchsia/private/workflows:fuchsia_task_verbs.bzl", "verbs")
load(
    "//fuchsia/private:providers.bzl",
    "FuchsiaEmulatorInfo",
    "FuchsiaLocalPackageRepositoryInfo",
    "FuchsiaProductBundleInfo",
)
load("//fuchsia/private/workflows:fuchsia_shell_task.bzl", "shell_task_rule")
load(":utils.bzl", "full_product_bundle_url")

def _filter_empty(tasks):
    return [t for t in tasks if t]

def fuchsia_development_configuration(
        name,
        preflight_tasks = None,
        postflight_tasks = None,
        package_repository = None,
        product_bundle = None,
        stop_emulators = False,
        autodetect_target = False,
        emulator = None):
    """Creates a configuration which can be used to prepare a development environment

    The development configuration is a target which describes how a user wants
    to setup their development environment for a given target. A user can call
    `bazel run :my_config` to set defaults and tear down old state.

    Checking the status of your environment:
    The status of a development environment can be checked by running the command
    `bazel run <name>.status`
    This command will query your system to see if the environment matches the expected
    environment.

    Args:
        name: The target name
        preflight_tasks: Any fuchsia_tasks which will run at the beginning of the workflow
        postflight_tasks: Any fuchsia_tasks which will run at the end of the workflow
        package_repository: A fuchsia_local package repository which will be created and
          made the default repository for publishing packages.
        product_bundle: A product bundle which will be fetched and registered with the
          target.
        stop_emulators: If True, all emulators will be stopped before this workflow runs.
        autodetect_target: If True, will attempt to automatically detect the default target.
        emulator: If provided, the emulator will be started and made the default target.
    """
    if autodetect_target:
        if emulator:
            fail("Development Configurations cannot launch an emulator and autodetect a target")
        if not product_bundle:
            fail("Development Configurations cannot autodetect a target without a product bundle")

    _status_check_task(
        name = name + ".status",
        emulator = emulator,
        package_repo = package_repository,
        product_bundle = product_bundle,
    )

    preflight_workflow = _pre_postflight_workflow(name, preflight_tasks, "preflight")
    pb_workflow = _product_bundle_workflow(product_bundle)
    emulator_workflow = _emulator_workflow(name, emulator, package_repository)
    autodetect_workflow = _autodetect_workflow(name, autodetect_target, package_repository, product_bundle)
    package_repo_workflow = _package_repository_workflow(name, package_repository)
    set_defaults_workflow = _set_defaults_workflow(name, emulator, package_repository)
    postflight_worfklow = _pre_postflight_workflow(name, postflight_tasks, "postflight")

    stop_emulators_task = None
    if stop_emulators:
        stop_emulators_task = name + ".stop_emulators"
        fuchsia_task_ffx(
            name = stop_emulators_task,
            arguments = [
                "emu",
                "stop",
                "--all",
            ],
        )

    # Start the package server
    start_server_task = name + ".start_package_server"
    _start_server_if_needed(
        name = start_server_task,
    )

    summary_task = name + ".summary"
    _summary_task(
        name = summary_task,
        product_bundle = product_bundle,
        emulator = emulator,
        package_repository = package_repository,
    )

    sequence = _filter_empty([
        stop_emulators_task,
        start_server_task,
        preflight_workflow,
        pb_workflow,
        package_repo_workflow,
        emulator_workflow,
        autodetect_workflow,
        # We need to set the defaults in their own workflow. This works around
        # an issue where ffx might not actually set the defaults.
        set_defaults_workflow,
        postflight_worfklow,
        summary_task,
    ])

    fuchsia_workflow(
        name = name,
        sequence = sequence,
    )

def _summary_task_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]

    summary_lines = [
        "-- Development Configuration Prepared --",
        "Using SDK Version: {}".format(sdk.sdk_id),
        "",
    ]

    if ctx.attr.product_bundle:
        pb_info = ctx.attr.product_bundle[FuchsiaProductBundleInfo]
        summary_lines.extend([
            "Product Bundle Summary:",
            " - is_remote = {}".format("True" if pb_info.is_remote else "False"),
            " - full URL: {}".format(full_product_bundle_url(ctx, pb_info)),
            " - local repository: {}".format(pb_info.repository),
            "",
        ])

    if ctx.attr.emulator:
        emulator_info = ctx.attr.emulator[FuchsiaEmulatorInfo]
        launch_options = [" {}".format(o) for o in emulator_info.launch_options]
        summary_lines.extend([
            "Emulator Summary:",
            " - name = {}".format(emulator_info.name),
            " - launch options:{}".format(",".join(launch_options)),
            "",
        ])

    if ctx.attr.package_repository:
        repo_info = ctx.attr.package_repository[FuchsiaLocalPackageRepositoryInfo]
        summary_lines.extend([
            "Package Repository Summary:",
            " - name = {}".format(repo_info.repo_name),
            " - path = {}".format(repo_info.repo_path),
            "",
        ])

    summary_text = ctx.actions.declare_file(ctx.label.name + ".summary_text")
    ctx.actions.write(
        output = summary_text,
        content = "\n".join(summary_lines) + "\n",
    )

    # Look into writing a script file for clang and then doing run
    clang_version = ctx.actions.declare_file(ctx.label.name + ".clang_version")
    ctx.actions.run_shell(
        outputs = [clang_version],
        tools = [ctx.executable._clang_bin],
        command = "%s --version > %s" %
                  (ctx.executable._clang_bin.path, clang_version.path),
    )

    return make_shell_task(
        command = [
            "cat",
            summary_text,
            "&&",
            "echo",
            "Clang Version: ",
            "&&",
            "cat",
            clang_version,
        ],
    )

(__summary_task, _summary_task_for_test, _summary_task) = shell_task_rule(
    implementation = _summary_task_impl,
    toolchains = [
        "@rules_fuchsia//fuchsia:toolchain",
    ],
    attrs = {
        "product_bundle": attr.label(providers = [[FuchsiaProductBundleInfo]]),
        "package_repository": attr.label(providers = [[FuchsiaLocalPackageRepositoryInfo]]),
        "emulator": attr.label(providers = [[FuchsiaEmulatorInfo]]),
        #TODO: don't require the clang binary.
        "_clang_bin": attr.label(
            default = "@fuchsia_clang//:bin/clang",
            executable = True,
            cfg = "exec",
            allow_single_file = True,
        ),
    },
)

def _pre_postflight_workflow(name, tasks, short_name):
    if not tasks or len(tasks) == 0:
        return None

    workflow_name = name + "." + short_name

    fuchsia_workflow(
        name = workflow_name,
        sequence = tasks,
    )
    return workflow_name

def _product_bundle_workflow(product_bundle):
    if not product_bundle:
        return None
    return verbs.fetch(product_bundle)

def _emulator_workflow(name, emulator, package_repository):
    if not emulator:
        return None

    workflow_name = name + ".emulator"
    sequence = [
        verbs.stop(emulator),
        verbs.start(emulator),
        verbs.wait(emulator),
    ]

    if package_repository:
        # Register the repo with the emulator
        register_task = name + ".register_package_repository"
        fuchsia_task_emulator_register_repository(
            name = register_task,
            repository = package_repository,
            emulator = emulator,
        )
        sequence.append(register_task)

    fuchsia_workflow(
        name = workflow_name,
        sequence = sequence,
    )

    return workflow_name

def _autodetect_workflow(name, autodetect_target, package_repository, product_bundle):
    # Check preconditions
    if not autodetect_target:
        return None

    workflow_name = name + ".autodetect"
    fuchsia_task_autodetect_target(
        name = workflow_name,
        product_bundle = product_bundle,
        package_repo = package_repository,
    )

    return workflow_name

def _package_repository_workflow(name, package_repo):
    if not package_repo:
        return None

    workflow_name = name + ".package_repository"
    sequence = [
        verbs.delete(package_repo),
        verbs.create(package_repo),
    ]

    fuchsia_workflow(
        name = workflow_name,
        sequence = sequence,
    )

    return workflow_name

def _set_defaults_workflow(name, emulator, package_repo):
    sequence = []
    if emulator:
        sequence.append(verbs.make_default(emulator))

    if package_repo:
        sequence.append(verbs.make_default(package_repo))

    if len(sequence) > 0:
        workflow_name = name + ".set_defaults"
        fuchsia_workflow(
            name = workflow_name,
            sequence = sequence,
        )

        return workflow_name
    else:
        return None

def _start_server_if_needed_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]
    return make_shell_task(
        command = [
            "[[",
            "$(",
            sdk.ffx,
            "config",
            "get",
            "repository.server.enabled",
            ")",
            "!=",
            "'true'",
            "]]",
            "&&",
            sdk.ffx,
            "repository",
            "server",
            "start",
            "||",
            "true",
        ],
    )

(
    __start_server_if_needed,
    _start_server_if_needed_for_test,
    _start_server_if_needed,
) = shell_task_rule(
    implementation = _start_server_if_needed_impl,
    toolchains = [
        "@rules_fuchsia//fuchsia:toolchain",
    ],
)

def _status_check_task_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]
    extra_args = []
    if ctx.attr.emulator:
        extra_args.extend([
            "--expected_emulator",
            ctx.attr.emulator[FuchsiaEmulatorInfo].name,
        ])

    if ctx.attr.package_repo:
        extra_args.extend([
            "--expected_package_repo",
            ctx.attr.package_repo[FuchsiaLocalPackageRepositoryInfo].repo_name,
        ])

    if ctx.attr.product_bundle:
        pb_info = ctx.attr.product_bundle[FuchsiaProductBundleInfo]
        extra_args.extend([
            "--expected_product_bundle",
            full_product_bundle_url(ctx, pb_info),
            "--expected_product_bundle_repo",
            pb_info.repository,
            "--expected_sdk_version",
            pb_info.version or ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"].sdk_id,
            "--expected_product_name",
            pb_info.product_name,
        ])

    return make_shell_task(
        command = [
            ctx.attr._development_status_tool,
            "--ffx",
            sdk.ffx,
            "--name",
            str(ctx.label).replace(".status", ""),
        ] + extra_args,
    )

(
    __status_check_task,
    _status_check_task_for_test,
    _status_check_task,
) = shell_task_rule(
    implementation = _status_check_task_impl,
    toolchains = [
        "@rules_fuchsia//fuchsia:toolchain",
    ],
    attrs = {
        "emulator": attr.label(providers = [[FuchsiaEmulatorInfo]]),
        "package_repo": attr.label(providers = [[FuchsiaLocalPackageRepositoryInfo]]),
        "product_bundle": attr.label(providers = [[FuchsiaProductBundleInfo]]),
        "_development_status_tool": attr.label(
            default = "//fuchsia/tools:development_status",
            doc = "The tool to dump the status.",
            executable = True,
            cfg = "target",
        ),
    },
)
