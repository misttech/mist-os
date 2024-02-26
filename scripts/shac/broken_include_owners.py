# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Checks OWNERS file for broken includes.
"""

import json
import re
import sys
import os


def main():
    owners_file = sys.argv[1]

    with open(owners_file, "r") as file:
        lines = file.readlines()

    # Temporary allowlist for third party files with broken includes
    # TODO(danikay) Delete when broken includes are removed
    allow_list = [
        "src/developer/forensics/crash_reports/OWNERS",
        "src/developer/forensics/exceptions/OWNERS",
        "third_party/rust_crates/vendor/regex-1.5.6/OWNERS",
        "third_party/rust_crates/vendor/zerocopy-0.8.0-alpha.1/OWNERS",
        "third_party/rust_crates/vendor/tracing-0.1.37/OWNERS",
    ]
    allow_list = [os.path.abspath(path) for path in allow_list]

    broken_includes = []

    for line_number, line in enumerate(lines, start=1):
        include_path = None
        # Search for "file:" or "include" keyword for importing OWNERS file
        match = re.search(r"(include\s|file:)\s*(\S+)", line)
        if match:
            include_path = match.group(2)

            if include_path.startswith("/"):
                include_path = include_path.lstrip("/")
                abs_path = os.path.abspath(include_path)
            else:
                dir_path = os.path.dirname(owners_file)
                abs_path = os.path.abspath(os.path.join(dir_path, include_path))

            if not os.path.exists(abs_path) and abs_path not in allow_list:
                broken_includes.append(line_number)

    print(json.dumps([{"lines": broken_includes}], indent=2))


if __name__ == "__main__":
    main()
