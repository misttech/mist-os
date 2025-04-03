#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""fuchsia_fortunes.py prints a random fortune each time.

Example usage: run this script once per ssh login.
"""
import random
import sys
from typing import Sequence

# Guidelines:
# Keep entries short, typically one or two lines.
# Use links.
_FORTUNE_SET = [
    # sections by topic:
    # General
    "Learn general productivity tips from go/engfortunes.",
    "Learn Fuchsia productivity tricks one tidbit at a time: go/fuchsia-fortunes.",
    # Build
    """Looking for 'fx build' support? go/fuchsia-build-chat""",
    """Slow build?  File a report at go/fx-build-slow.
Include a link to data from 'fx report-last-build'.""",
    """Accelerate builds with remote execution and caching:
https://fuchsia.dev/internal/intree/concepts/remote-builds""",
    """Profile memory and network usage of builds with 'fx build-profile enable'""",
    # fx
    """Help with fx commands: fx help COMMAND""",
    # ffx
    # editors
    # Infra
    # testing
    # debugging
    # add more helpful entries here...
]


def main(argv: Sequence[str]) -> int:
    print(random.choice(_FORTUNE_SET))
    return 0


if __name__ == "__main__":
    main(sys.argv[1:])
