# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This tool is for invoking by the CTF trybot.  It is intended for
# comparing the FIDL methods that were exercised in CTF tests
# to the FIDL methods that exist in the SDK.

import sys

if __name__ == "__main__":
    sys.stdout.write(f'{{ "Hello": "world", "request": "{sys.argv[1]}"}}\n')
