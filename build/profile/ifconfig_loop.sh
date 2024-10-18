#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script calls ifconfig in a loop in between timestamps
# so that the data series can be formatted into a chrome-trace
# using ifconfig_trace.py.

function usage() {
  cat <<EOF
$0 [script_options] -- [ifconfig_options]
EOF
}

# interval in seconds
interval=2

prev_opt=
for opt
do
  # handle --option arg
  if [[ -n "$prev_opt" ]]
  then
    eval "$prev_opt"=\$opt
    prev_opt=
    shift
    continue
  fi

  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac

  case "$opt" in
    -n) prev_opt=interval ;;
    -n=*) interval="$optarg" ;;
    --) shift ; break ;;
    *) echo "Error: unknown option $opt" ; exit 1 ;;
  esac
  shift
done

ifconfig_args=("$@")

# Run until interrupted.
while true
do
  date="$(date "+%Y-%m-%d %H:%M:%S")"
  echo "TIME: $date"
  ifconfig "${ifconfig_args[@]}"
  sleep "$interval"s
done
