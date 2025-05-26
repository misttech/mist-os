# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Abstract base class for Starnix affordance."""

import abc

from honeydew.affordances import affordance


class Starnix(affordance.Affordance):
    """Abstract base class for Starnix affordance."""

    @abc.abstractmethod
    def run_console_shell_cmd(self, cmd: list[str]) -> str:
        """Run a starnix console command and return its output.

        Args:
            cmd: cmd that need to be run excluding `starnix /bin/sh -c`.

        Returns:
            Output of `ffx -t {target} starnix /bin/sh -c {cmd}`.

        Raises:
            starnix_errors.StarnixError: In case of starnix command failure.
            errors.NotSupportedError: If Fuchsia device does not support Starnix.
        """
