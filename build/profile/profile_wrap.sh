#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Collect system profile information from tools like vmstat and ifconfig
# for the duration of a wrapped command, logging outputs to file.
# This also produces a chrome-tracing formatted version of the
# same data in the log file name-suffixed with .json.

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"
readonly script_basename="${script##*/}"

readonly vmstat_trace_tool="$script_dir/vmstat_trace.py"
readonly ifconfig_loop="$script_dir/ifconfig_loop.sh"
readonly ifconfig_trace_tool="$script_dir/ifconfig_trace.py"

function usage() {
  cat <<EOF
Usage: $script \
  --vmstat-log vmstat_logfile \
  --ifconfig-log ifconfig_logfile \
  [script_args] -- command...

to run a self-test:
  $script --self-test
EOF
}

self_test=0
vmstat_logfile=
ifconfig_logfile=

vmstat="$(which vmstat)" || {
  echo "Unable to find vmstat tool.  Exiting."
  exit 1
}
readonly vmstat_base="$(basename "$vmstat")"

ifconfig="$(which ifconfig)" || {
  echo "Unable to find ifconfig tool.  Exiting."
  exit 1
}
readonly ifconfig_base="$(basename "$ifconfig_loop")"

# extract script options first
# vmstat: always include the timestamp (-t), expected by vmstat_trace.py.
vmstat_args=(-t)
ifconfig_args=()
interval=2  # seconds
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
    --ifconfig-log) prev_opt=ifconfig_logfile ;;
    --ifconfig-log=*) ifconfig_logfile="$optarg" ;;
    --vmstat-log) prev_opt=vmstat_logfile ;;
    --vmstat-log=*) vmstat_logfile="$optarg" ;;
    -n) prev_opt=interval ;;
    -n=*) interval="$optarg" ;;
    --vmstat-arg=*) vmstat_args+=( "$optarg" ) ;;
    --ifconfig-arg=*) ifconfig_args+=( "$optarg" ) ;;
    --self-test) self_test=1 ;;
    --) shift ; break ;;
    *) echo "Unknown $0 option: $opt" ; usage ; exit 1 ;;
  esac
  shift
done

[[ "$self_test" == 1 ]] || [[ -n "$vmstat_logfile" ]] || [[ -n "$ifconfig_logfile" ]] || {
  echo "At least one of (--vmstat-log, --ifconfig-log) is required."
  exit 1
}

# Everything else after '--' is the command to run.
cmd=("$@")

[[ "$self_test" == 1 ]] || [[ "$#" > 0 ]] || { echo "Missing command to run (after --)." ; exit 1; }

function shell_subprocesses() {
  ps --ppid "$$"
}

# Find the subprocess of this shell for 'vmstat' and 'ifconfig'.
# Can't use '$!' because that points to the last command in the pipe chain.
function subprocess_pid() {
  tool_basename="$1"
  # ps displays only the first 15 characters of the executable.
  shell_subprocesses | grep -w "${tool_basename:0:15}" | cut -d\  -f 1
}

function run_self_test() {
  echo "Running self-tests only."
  echo "==== Testing subprocess_pid()..."
  echo "ps is $(which ps), version:"
  ps --version
  echo "'sleep' in the background"
  sleep 10s &
  echo "subprocesses of parent shell (pid=$$) are:"
  shell_subprocesses
  local sleep_pid="$(subprocess_pid "sleep")"
  if [[ -z "$sleep_pid" ]]
  then
    echo "Unable to find subprocess pid for 'sleep'."
    return 1
  else
    echo "Got a sleep pid: $sleep_pid"
    kill "$sleep_pid"
    echo "  PASSED"
  fi
}

if [[ "$self_test" == 1 ]]
then
  test_status=0
  run_self_test || {
    test_status="$?"
    echo "*** Report test failures to go/fuchsia-build-bug."
  }
  exit "$test_status"
fi

pids_not_found=0
shutdown_pids=()

# Tag json with the unique fx build id, if it is set.  Print to stdout.
trace_preamble_args=()
[[ -z "${FX_BUILD_UUID+x}" ]] || trace_preamble_args=( --metadata="FX_BUILD_UUID:$FX_BUILD_UUID" )

if [[ -n "$vmstat_logfile" ]]
then
  rm -f "$vmstat_logfile" "$vmstat_logfile.json"
  touch "$vmstat_logfile"

  # Launch vmstat in the background, along with any output scanners.
  "$vmstat" "${vmstat_args[@]}" "$interval" | \
    tee "$vmstat_logfile" | \
    "$vmstat_trace_tool" "${trace_preamble_args[@]}" - \
    > "$vmstat_logfile.json" &

  # Terminating the 'vmstat' process should cascade to the other downstream
  # processes via SIGPIPE.
  readonly vmstat_pid="$(subprocess_pid "$vmstat_base")"
  if [[ -n "$vmstat_pid" ]]
  then shutdown_pids+=( "$vmstat_pid" )
  else pids_not_found=1
  fi
fi

if [[ -n "$ifconfig_logfile" ]]
then
  rm -f "$ifconfig_logfile" "$ifconfig_logfile.json"
  touch "$ifconfig_logfile"

  # Launch ifconfig in the background, along with any output scanners.
  "$ifconfig_loop" -n "$interval" "${ifconfig_args[@]}" | \
    tee "$ifconfig_logfile" | \
    "$ifconfig_trace_tool" "${trace_preamble_args[@]}" - \
    > "$ifconfig_logfile.json" &

  # Terminating the 'ifconfig_loop.sh' process should cascade to its
  # subprocess and the rest of the pipe.
  readonly ifconfig_pid="$(subprocess_pid "$ifconfig_base")"
  if [[ -n "ifconfig_pid" ]]
  then shutdown_pids+=( "$ifconfig_pid" )
  else pids_not_found=1
  fi
fi

# Terminate vmstat and ifconfig when main command is complete (or interrupted).
function shutdown() {
  if [[ "$pids_not_found" > 0 ]]
  then cat <<EOF
[$script] Warning: Unable to find at least one of the backgrounded subprocesses
that need to be terminated, so some residual subprocesses may still be running.
Look for potentially unwanted subprocesses to kill with 'ps T'.
See also known issue b/375201428.
If this issue persists, you can:
  fx build-profile disable
  file a go/fuchsia-build-bug
EOF
  fi
  if [[ "${#shutdown_pids[@]}" > 0 ]]
  then kill "${shutdown_pids[@]}"
  fi
}
trap shutdown EXIT

# Run the wrapped command.
cmd_status=0
"${cmd[@]}" || cmd_status="$?"
exit "$cmd_status"
