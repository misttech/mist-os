#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Runs vmstat for the duration of a wrapped command,
# logging its output to file.

function usage() {
  cat <<EOF
Usage:
$0 -o logfile [vmstat_args] -- command...
EOF
}

logfile=

vmstat="$(which vmstat)" || {
  echo "Unable to find vmstat tool.  Exiting."
  exit 1
}

# extract script options and vmstat_args first
vmstat_args=()
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
    -o) prev_opt=logfile ;;
    -o=*) logfile="$optarg" ;;
    --) shift ; break ;;
    *) vmstat_args+=( "$opt" ) ;;
  esac
  shift
done

[[ -n "$logfile" ]] || { echo "Missing required -o LOGFILE"; exit 1;}

# Everything else after '--' is the command to run.
cmd=("$@")

rm -f "$logfile"
touch "$logfile"
"$vmstat" "${vmstat_args[@]}" > "$logfile" &
vmstat_pid="$!"

cmd_status=0

# No need to kill the vmstat process inside a trap function,
# the kill signals will already propagate to the vmstat process,
# and the default termination is what we want.

"${cmd[@]}" || cmd_status="$?"
kill -INT "$vmstat_pid"
exit "$cmd_status"
