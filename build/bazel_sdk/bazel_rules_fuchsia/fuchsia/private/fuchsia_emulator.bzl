# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    "//fuchsia/private/workflows:fuchsia_emulator_tasks.bzl",
    "fuchsia_task_emulator_wait",
    "fuchsia_task_make_default_emulator",
    "fuchsia_task_reboot_emulator",
    "fuchsia_task_start_emulator",
    "fuchsia_task_stop_emulator",
)
load("//fuchsia/private:providers.bzl", "FuchsiaEmulatorInfo")
load("//fuchsia/private/workflows:fuchsia_task_verbs.bzl", "make_help_executable", "verbs")

def fuchsia_emulator(
        name,
        product_bundle,
        launch_options = None,
        emulator_name = None,
        **kwargs):
    """Describes a fuchsia_emulator instance and the tasks which control it.

    The following tasks will be created:
    - name.start: Starts the emulator
    - name.stop: Stops the emulator
    - name.reboot: Reboots the emulator
    - name.wait: waits for the emulator to come online.
    - name.make_default: Makes the emulator the default.

    Args:
        name: The target name.
        emulator_name: The name of the emulator. Defaults to name.
        product_bundle: The product bundle associated with this emulator.
        launch_options: Additional options to launch the emulator with.
        **kwargs: Extra attributes to pass along to the build rule.
    """
    _fuchsia_emulator(
        name = name,
        emulator_name = emulator_name,
        launch_options = launch_options,
        **kwargs
    )

    fuchsia_task_start_emulator(
        name = verbs.start(name),
        emulator = name,
        product_bundle = product_bundle,
        **kwargs
    )

    fuchsia_task_stop_emulator(
        name = verbs.stop(name),
        emulator = name,
        **kwargs
    )

    fuchsia_task_reboot_emulator(
        name = verbs.reboot(name),
        emulator = name,
        **kwargs
    )

    fuchsia_task_emulator_wait(
        name = verbs.wait(name),
        emulator = name,
        **kwargs
    )

    fuchsia_task_make_default_emulator(
        name = verbs.make_default(name),
        emulator = name,
        **kwargs
    )

def _fuchsia_emulator_impl(ctx):
    return [
        DefaultInfo(executable = make_help_executable(
            ctx,
            {
                verbs.start: "Starts the emulator",
                verbs.stop: "Stops the emulator",
                verbs.reboot: "Reboots the emulator",
                verbs.wait: "Waits for the emulator to come online",
                verbs.make_default: "Make the emulator the default",
            },
        )),
        FuchsiaEmulatorInfo(
            name = ctx.attr.emulator_name or ctx.label.name,
            launch_options = ctx.attr.launch_options,
        ),
    ]

_fuchsia_emulator = rule(
    implementation = _fuchsia_emulator_impl,
    doc = "A rule describing a fuchsia emulator.",
    attrs = {
        "emulator_name": attr.string(
            doc = "What to name the emulator, defaults to the target name",
        ),
        "launch_options": attr.string_list(
            doc = "A list of additional options to start the emulator with",
            default = [],
        ),
    },
    executable = True,
)
