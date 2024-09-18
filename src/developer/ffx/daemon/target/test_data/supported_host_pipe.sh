#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
set -e

# Test script to respond as a supported host pipe abi revision

# Expected command line: "-F none -o CheckHostIP=no -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=1 -o ServerAliveCountMax=20 -o LogLevel=ERROR --i /some/path/fuchsia_ed25519 -p 22 192.168.1.1 remote_control_runner --circuit 7561919583897036558 --abi-revision 692809671562192090\n"

# Read args. Skip ones that are not important.
REMOTE_CONTROL_RUNNER="remote_control_runner"
OTHER_ARGS=()
pipe_command=""
tool_revision=""

while [[ $# -gt 0 ]]; do
  case $1 in
    "$REMOTE_CONTROL_RUNNER")
      pipe_command="$1"
      shift;;
    --abi-revision)
        tool_revision="$2"
      shift # past argument
      shift # past value
      ;;
    *)
      OTHER_ARGS+=("$1") # save  arg
      shift # past argument
      ;;
  esac
done

if [ "$pipe_command" != $REMOTE_CONTROL_RUNNER ]; then
    echo "unknown command $pipe_command" >&2
    exit 1
fi

# accept any revision but 0
if [ "$tool_revision" != 0 ]; then
    echo "{\"ssh_connection\": \"10.0.2.2 43060 10.0.2.15 22\", \"compatibility\":{ \"status\": \"OK\", \"platform_abi\": \"692809671562192090\", \"message\": \"Daemon is running supported revision\" }}"
else
 echo "{\"ssh_connection\": \"10.0.2.2 43060 10.0.2.15 22\", \"compatibility\":{ \"status\": \"OK\", \"platform_abi\": \"692809671562192090\", \"message\": \"Daemon is running supported revision\" }}"
fi
exit 0
