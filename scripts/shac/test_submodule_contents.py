# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Print contents of the test submodule's README."""

import pathlib
import sys


def main() -> None:
    path = pathlib.Path(sys.argv[1])
    if path.exists():
        print(path.read_text())
    else:
        print(f"{path} does not exist")


if __name__ == "__main__":
    main()
