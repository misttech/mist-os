#!/bin/bash
# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

if [[ -z $FUCHSIA_GCE_PROJECT ]]; then
  source "$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"/env.sh
fi

gcloud -q compute instances delete $FUCHSIA_GCE_INSTANCE --zone=$FUCHSIA_GCE_ZONE &

# This command takes a fairly long time (~3 minutes) and is silent, so
# print out a spinner until it's done to indicate to the developer that
# we're still working.
PID=$!
i=1
sp="/|\\-/|\\-"
echo -n "Deletion in progress: "
while kill -0 "$PID" 2>/dev/null
do
  printf "\b${sp:i++%${#sp}:1}"
  sleep 1
done
echo "Done."
