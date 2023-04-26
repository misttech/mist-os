#!/bin/sh
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

ffx component run >/dev/null 2>/dev/null \
    /core/starnix_runner/playground:starmium \
    fuchsia-pkg://fuchsia.com/starmium#meta/chromiumos_container.cm \

set -e

ffx starnix console -m /core/starnix_runner/playground:starmium
