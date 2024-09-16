#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
set -e

# Test script to respond as a legacyhost pipe that does not support abi revision

# Expected command line: "-F none -o CheckHostIP=no -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=1 -o ServerAliveCountMax=20 -o LogLevel=ERROR -i /some/path/fuchsia_ed25519 -p 22 192.168.1.1 remote_control_runner --circuit 7561919583897036558 --abi-revision 692809671562192090\n"

# Read args. Skip ones that are not important.
REMOTE_CONTROL_RUNNER="remote_control_runner"
OTHER_ARGS=()
pipe_command=""
connection=""
while [[ $# -gt 0 ]]; do
  case $1 in
    "$REMOTE_CONTROL_RUNNER")
      pipe_command="$1"
      shift;;
    --abi-revision)
        echo "Unrecognized argument: --abi-revision" >&2
        exit 1
      ;;
    echo)
        connection="$2"
        # 3 is &&
        shift
        shift
        shift
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

if [ "$connection" = "++ \$SSH_CONNECTION ++" ]; then
    echo "++ 10.0.2.2 43060 10.0.2.15 22 ++"
fi

exit 0
