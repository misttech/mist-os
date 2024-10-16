#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Runs vmstat for the duration of a wrapped command,
# logging its output to file.
# This also produces a chrome-tracing formatted version of the
# same data in the log file name-suffixed with .json.

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"
readonly script_basename="${script##*/}"

readonly trace_tool="$script_dir/vmstat_trace.py"

function usage() {
  cat <<EOF
Usage:
$script -o logfile [vmstat_args] -- command...
EOF
}

logfile=

vmstat="$(which vmstat)" || {
  echo "Unable to find vmstat tool.  Exiting."
  exit 1
}

# extract script options and vmstat_args first
# Always include the timestamp, expected by vmstat_trace.py.
vmstat_args=(-t)
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

rm -f "$logfile" "$logfile.json"
touch "$logfile"
# Launch vmstat in the background, along with any of its output scanners.
"$vmstat" "${vmstat_args[@]}" | tee "$logfile" | "$trace_tool" - > "$logfile.json" &

vmstat_base="$(basename "$vmstat")"
# Find the subprocess of this shell for 'vmstat'.
# Can't use '$!' because that points to the last command in the pipe chain.
# Terminating the 'vmstat' process should cascade to the other downstream
# processes via SIGPIPE.
vmstat_pid="$(ps --ppid "$$" | grep -w "$vmstat_base" | cut -d\  -f 1)"

# Terminate vmstat when main command is complete (or interrupted).
function shutdown() {
  kill "$vmstat_pid"
}
trap shutdown EXIT

# Run the wrapped command.
cmd_status=0
"${cmd[@]}" || cmd_status="$?"
exit "$cmd_status"
