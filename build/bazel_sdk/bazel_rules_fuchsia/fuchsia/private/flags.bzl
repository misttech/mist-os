# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("//fuchsia/private:providers.bzl", "FuchsiaPackageRepoPathInfo")

def _package_repo_path_flag_impl(ctx):
    path = ctx.build_setting_value
    return FuchsiaPackageRepoPathInfo(path = path)

package_repo_path_flag = rule(
    implementation = _package_repo_path_flag_impl,
    build_setting = config.string(flag = True),
    doc = """
        A string-typed build setting that can be set on the command line to
        specify the path to the package repository.
    """,
)
