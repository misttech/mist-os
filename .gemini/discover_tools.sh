#!/bin/bash
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Tool Discovery Script
#
# This script outputs a JSON array of FunctionDeclaration objects to stdout.
# Gemini CLI will call this script to discover custom tools.

# Define all your tools here.
# To add a new tool, simply add another JSON object to the array.
read -r -d '' TOOLS_JSON <<EOF
[
  {
    "name": "search_file_content",
    "description": "Custom implementation: Searches for a regular expression pattern within the content of files using `git grep` and returns the exact output.",
    "parameters": {
      "type": "OBJECT",
      "properties": {
        "pattern": {
          "type": "STRING",
          "description": "The regular expression (regex) to search for."
        },
        "path": {
          "type": "STRING",
          "description": "An optional path to a directory to search within. Defaults to the current directory."
        }
      },
      "required": ["pattern"]
    }
  }
]
EOF

echo "$TOOLS_JSON"