# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Shac config file for bazel_rules_fuchsia.

This is mainly for executing the buildifier linter on the package, so
it doesn't trip linting tools in external projects.

Enabling this check on all of fuchsia.git will require updating 300+ files
across the tree, so that will happen in the future
(https://fxbug.dev/333477886)"""

load("//third_party/shac-project/checks-starlark/src/register.star", "register_buildifier_lint")

register_buildifier_lint(
    tool = "../../../prebuilt/third_party/buildifier/linux-x64/buildifier",
    emit_message = "Please address linter issues.",
)

# TODO(https://fxbug.dev/333478321): "shac fmt" complains if a shac.star file
# does not register any formatter checks.
# buildifier: disable=unused-variable
def _no_op(ctx):
    pass

shac.register_check(shac.check(_no_op, formatter = True))
