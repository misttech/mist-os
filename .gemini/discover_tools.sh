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
    "name": "git_ls_files",
    "description": "Custom implementation: List files in a git repository directory using \`git ls-files\` and returns the exact output.",
    "parameters": {
      "type": "OBJECT",
      "properties": {
        "files": {
          "type": "ARRAY",
          "items": { "type": "STRING" },
          "description": "The regular expression (regex) to search for. Defaults to all files"
        },
        "path": {
          "type": "STRING",
          "description": "An optional path to a directory to search within. Defaults to the current directory."
        }
      },
      "required": ["path"]
    }
  },
  {
    "name": "jiri_grep",
    "description": "Custom implementation: Searches for a regular expression pattern across all repositories using \`jiri grep\` and returns the exact output.",
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
