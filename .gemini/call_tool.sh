#!/bin/bash
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Tool Call Script
#
# This script receives the tool name as the first argument ($1)
# and the tool's arguments as a JSON object on stdin.
# It must return a JSON object with an "output" key on stdout.

TOOL_NAME="$1"

# Read the JSON arguments from stdin
read -r ARGS_JSON

# Use a case statement to handle different tools.
# This makes the script easily extensible for future tools.
case "$TOOL_NAME" in
  "search_file_content")
    # Extract parameters using jq
    PATTERN=$(echo "$ARGS_JSON" | jq -r '.pattern')
    SEARCH_PATH=$(echo "$ARGS_JSON" | jq -r '.path // "."') # Default to current dir if null

    # Run the git grep command.
    # The `( ... )` subshell ensures that `cd` doesn't affect the script's working directory.
    OUTPUT=$( (cd "$SEARCH_PATH" && git grep -n "$PATTERN") 2>&1 )

    # Return the exact output in the required JSON format.
    jq -n --arg output "$OUTPUT" '{"output": $output}'
    ;;

  # To add a new tool, add another case here:
  # "my_other_tool")
  #   ... logic for my_other_tool ...
  #   ;;

  *)
    jq -n --arg tool_name "$TOOL_NAME" '{"output": "Error: Unknown tool name '\''\($tool_name)'\''"}'
    exit 1
    ;;
esac