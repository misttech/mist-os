# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

FORMATTER_MSG = "File not formatted. Run `fx format-code` to fix."

def cipd_platform_name(ctx):
    """Returns CIPD's name for the current host platform.

    This is the platform name that appears in most prebuilt paths.
    """
    os = {
        "darwin": "mac",
    }.get(ctx.platform.os, ctx.platform.os)
    arch = {
        "amd64": "x64",
    }.get(ctx.platform.arch, ctx.platform.arch)
    return "%s-%s" % (os, arch)
