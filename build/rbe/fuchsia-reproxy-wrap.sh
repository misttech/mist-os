#!/bin/bash
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# This script is a general purpose wrapper that automatically
# starts and stops 'reproxy' (re-client) around the wrapped command.
# Common uses of this include:
#   * Wrapping around build/ninja invocations that issue commands
#     that use 'rewrapper' (re-client).
#   * Wrapping around manual commands involving 'rewrapper'.
#
# See usage().

set -e

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"  # dirname

source "$script_dir"/common-setup.sh

readonly PREBUILT_OS="$_FUCHSIA_RBE_CACHE_VAR_host_os"
readonly PREBUILT_ARCH="$_FUCHSIA_RBE_CACHE_VAR_host_arch"

# The project_root must cover all inputs, prebuilt tools, and build outputs.
# This should point to $FUCHSIA_DIR for the Fuchsia project.
# ../../ because this script lives in build/rbe.
# The value is an absolute path.
project_root="$default_project_root"
project_root_rel="$(relpath . "$project_root")"

# defaults
readonly default_config="$script_dir"/fuchsia-reproxy.cfg
readonly gcertauth_config="$script_dir"/fuchsia-reproxy-gcertauth.cfg

readonly PREBUILT_SUBDIR="$PREBUILT_OS"-"$PREBUILT_ARCH"

readonly check_loas_script="$script_dir"/check_loas_restrictions.sh
readonly build_summary_script="$script_dir"/build_summary.py

# location of reclient binaries relative to output directory where build is run
reclient_bindir="$project_root_rel"/prebuilt/proprietary/third_party/reclient/"$PREBUILT_SUBDIR"

# Configuration for RBE metrics and logs collection.
readonly fx_build_metrics_config="$project_root_rel"/.fx-build-metrics-config

readonly jq="$project_root/prebuilt/third_party/jq/"$PREBUILT_SUBDIR"/bin/jq"

loas_type=auto

usage() {
  cat <<EOF
$script [reproxy options] -- command [args...]

This script runs reproxy around another command(s).
reproxy is a proxy process between reclient and a remote back-end (RBE).
It needs to be running to host rewrapper commands, which are invoked
by a build system like 'make' or 'ninja'.

example:
  $script -- ninja

options:
  --cfg FILE: reclient configs for reproxy [repeatable, cumulative]
    (default: $default_config)
  --bindir DIR: location of reproxy tools
  --logdir DIR: unique reproxy log dir
  --tmpdir DIR: reproxy temp dir
  --loas-type TYPE: {skip,auto,restricted,unrestricted}, default [$loas_type]
    'skip' will bypass any preflight authentication checks
    'auto' will attempt to detect as restricted or unrestricted.
  -t: print additional timestamps for measuring overhead.
  -v | --verbose: print events verbosely
  All other flags before -- are forwarded to the reproxy bootstrap.

environment variables:
  FX_BUILD_RBE_STATS: set to 1 to print a summary of RBE actions
    after the build (caching, races, downloads...).
  FX_REMOTE_BUILD_METRICS: set to 0 to skip anything related to RBE metrics
    This was easier than plumbing flags through all possible paths
    that call 'fx build'.
EOF
}

configs=()
reproxy_logdir=
reproxy_tmpdir=
verbose=0
print_times=0
bootstrap_options=()
prev_opt=
prev_opt_append=
# Extract script options before --
for opt
do
  # handle --option arg
  if test -n "$prev_opt"
  then
    eval "$prev_opt"=\$opt
    prev_opt=
    shift
    continue
  fi

  # handle cumulative args
  if test -n "$prev_opt_append"
  then
    eval "$prev_opt_append"+=\(\$opt\)
    prev_opt_append=
    shift
    continue
  fi

  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac

  case "$opt" in
    --cfg=*) configs+=("$optarg") ;;
    --cfg) prev_opt_append=configs ;;
    --bindir=*) reclient_bindir="$optarg" ;;
    --bindir) prev_opt=reclient_bindir ;;
    --logdir=*) reproxy_logdir="$optarg" ;;
    --logdir) prev_opt=reproxy_logdir ;;
    --tmpdir=*) reproxy_tmpdir="$optarg" ;;
    --tmpdir) prev_opt=reproxy_tmpdir ;;
    --loas-type=*) loas_type="$optarg" ;;
    --loas-type) prev_opt=loas_type ;;
    -t) print_times=1 ;;
    -v | --verbose) verbose=1 ;;
    # stop option processing
    --) shift; break ;;
    # Forward all other options to reproxy
    *) bootstrap_options+=("$opt") ;;
  esac
  shift
done
test -z "$prev_out" || { echo "Option is missing argument to set $prev_opt." ; exit 1;}

[[ "${#configs[@]}" > 0 ]] || configs=( "$default_config" )

function _timetrace() {
  [[ "$print_times" == 0 ]] || timetrace "$@"
}

_timetrace "main start (after option processing)"

readonly bootstrap="$reclient_bindir"/bootstrap
readonly reproxy="$reclient_bindir"/reproxy

# Generate unique dirs per invocation.
readonly date="$(date +%Y%m%d-%H%M%S)"
# Default location, when log/tmp dirs are unspecified.
readonly build_subdir=out/_unknown

[[ -n "$reproxy_logdir" ]] || {
  # 'mktemp -p' still yields to TMPDIR in the environment (bug?),
  # so override TMPDIR instead.
  reproxy_logdir="$(mktemp -d -t "reproxy.$date.XXXX")"
}
readonly _log_base="${reproxy_logdir##*/}"  # basename
[[ -n "$reproxy_tmpdir" ]] || {
  reproxy_tmpdir="$project_root/$build_subdir"/.reproxy_tmpdirs/"$_log_base"
}

readonly _fake_tmpdir="$(mktemp -u)"
readonly _tmpdir="${_fake_tmpdir%/*}"  # dirname

# The socket file doesn't need to be co-located with logs.
# Using an absolute path to the socket allows rewrapper to be invoked
# from different working directories.
readonly socket_path="$_tmpdir/$_log_base.sock"
test "${#socket_path}" -le 100 || {
  cat <<EOF
Socket paths are limited to around 100 characters on some platforms.
  Got: $socket_path (${#socket_path} characters)
See https://unix.stackexchange.com/questions/367008/why-is-socket-path-length-limited-to-a-hundred-chars
EOF
  exit 1
}
readonly server_address="unix://$socket_path"

# deps cache dir should be somewhere persistent between builds,
# and thus, not random.  /var/cache can be root-owned and not always writeable.
if test -n "$HOME"
then _RBE_cache_dir="$HOME/.cache/reproxy/deps"
else _RBE_cache_dir="/tmp/.cache/reproxy/deps"
fi
mkdir -p "$_RBE_cache_dir"

[[ "${USER-NOT_SET}" != "NOT_SET" ]] || {
  echo "Error: USER must be set to authenticate using RBE."
  exit 1
}

# These environment variables take precedence over those found in --cfg.
# These values are all dynamic and are short-lived, so it is better
# to pass them as environment variable than to write them out to
# an auxiliary config file.
bootstrap_env=(
  env
  TMPDIR="$reproxy_tmpdir"
  RBE_proxy_log_dir="$reproxy_logdir"
  RBE_log_dir="$reproxy_logdir"
  RBE_server_address="$server_address"
  # rbe_metrics.{pb,txt} appears in -output_dir
  RBE_output_dir="$reproxy_logdir"
  RBE_cache_dir="$_RBE_cache_dir"
)

if [[ "${FX_BUILD_UUID-NOT_SET}" == "NOT_SET" ]]
then
  build_uuid=$("$python" -S -c 'import uuid; print(uuid.uuid4())')
else
  build_uuid="${FX_BUILD_UUID}"
fi

# These environment variables take precedence over those found in --cfg.
readonly rewrapper_log_dir="$reproxy_logdir/rewrapper-logs"
mkdir -p "$rewrapper_log_dir"
rewrapper_env=(
  env
  RBE_server_address="$server_address"
  # rewrapper logs
  RBE_log_dir="$rewrapper_log_dir"
  # Technically, RBE_proxy_log_dir is not needed for rewrapper,
  # however, the reproxy logs do contain information about individual
  # rewrapper executions that could be discovered and used in diagnostics,
  # so we include it.
  RBE_proxy_log_dir="$reproxy_logdir"
  # Identify which build each remote action request comes from.
  RBE_invocation_id="user:$build_uuid"
)

# Check authentication.
# Same as 'fx rbe preflight', but re-implemented here to be standalone.
[[ "$loas_type" != "auto" ]] || {
  # Detect "restricted" or "unrestricted"
  loas_type="$("$check_loas_script" | tail -n 1)" || {
    echo "Error detecting LOAS certificate type"
    exit 1
  }
}

case "$loas_type" in
  skip) ;;
  unrestricted)
    # Eligible to use credential helper to refresh OAuth from LOAS.
    gcertstatus --check_remaining=2h > /dev/null || gcert || {
      echo "Please run gcert to get a valid LOAS certificate."
      exit 1
    }
    # There may be an reclient bootstrap bug where passing
    # reproxy config options does not get forwarded properly,
    # but concatenating configs together works.
    configs+=( "$gcertauth_config" )
    ;;
  *)  # including 'restricted'
    [[ "$loas_type" == restricted ]] || {
      cat <<EOF
Warning: Unexpected loas_type: '$loas_type'
Proceeding as if type is "restricted".
File a go/fuchsia-build-bug, including a go/paste link of: sh -x $check_loas_script
EOF
    }
    # Can only use OAuth tokens directly, using gcloud authentication.
    gcloud="$(which gcloud)" || {
      cat <<EOF
\`gcloud\` command not found (but is needed to authenticate).

Run 'fx rbe auth' for a first-time setup or follow steps at:
  http://go/cloud-sdk#installing-and-using-the-cloud-sdk

EOF
      exit 1
    }

    # Instruct user to authenticate if needed.
    "$gcloud" auth list 2>&1 | grep -q -w "$USER@google.com" || {
      cat <<EOF
Did not find credentialed account (\`gcloud auth list\`): $USER@google.com.

To authenticate, run 'fx rbe auth' or
  gcloud auth login --update-adc

EOF
      exit 1
    }
    ;;
esac

# If configured, collect reproxy logs.
BUILD_METRICS_ENABLED=0
if [[ "$FX_REMOTE_BUILD_METRICS" == 0 ]]
then echo "Disabled RBE metrics for this run."
else
  if [[ -f "$fx_build_metrics_config" ]]
  then source "$fx_build_metrics_config"
    # This config sets BUILD_METRICS_ENABLED.
  fi
fi

# reproxy wants temporary space on the same device where
# the build happens.  The default $TMPDIR is not guaranteed to
# be on the same physical device.
# Re-use the randomly generated dir name in a custom tempdir.
reproxy_tmpdir="$project_root/$build_subdir"/.reproxy_tmpdirs/"$_log_base"
mkdir -p "$reproxy_tmpdir"

function cleanup() {
  rm -rf "$reproxy_tmpdir"
}

# Honor additional reproxy configs and overrides.
bootstrap_reproxy_cfg="${configs[0]}"
if [[ "${#configs[@]}" -gt 1 ]]
then
  # If needed, concatenate multiple reproxy configs to a single file.
  bootstrap_reproxy_cfg="$reproxy_logdir/joined_reproxy.cfg"
  cat "${configs[@]}" > "$bootstrap_reproxy_cfg"
  [[ "$verbose" != 1 ]] || {
    echo "concatenated reproxy cfg: $bootstrap_reproxy_cfg"
  }
fi

# Startup reproxy.
# Use the same config for bootstrap as for reproxy.
# This also checks for authentication, and prompts the user to
# re-authenticate if needed.
_timetrace "Bootstrapping reproxy"
bootstrap_status=0
"${bootstrap_env[@]}" \
  "$bootstrap" \
  --re_proxy="$reproxy" \
  --cfg="$bootstrap_reproxy_cfg" \
  "${bootstrap_options[@]}" > "$reproxy_logdir"/bootstrap.stdout 2>&1 || bootstrap_status="$?"
# Silence, unless --verbose or there is an error.
[[ "$bootstrap_status" == 0 && "$verbose" != 1 ]] || {
  cat "$reproxy_logdir"/bootstrap.stdout
  cat <<EOF

build id: $build_uuid
logs: $reproxy_logdir
socket: $socket_path
EOF
}
_timetrace "Bootstrapping reproxy (done)"
[[ "$bootstrap_status" == 0 ]] || {
  # Check for possible known causes of errors, and remedies.
  if grep -q -e "credential" -e "authenticat" "$reproxy_logdir/bootstrap.ERROR"
  then
    cat <<EOF
Seeing RBE authentication issues?  Try 'fx rbe auth'.

If persistent errors look like b/358163278, try b/338509252#comment15.

If needed, file a support ticket at go/fx-build-bug.
EOF
  fi
  exit "$bootstrap_status"
}

test "$BUILD_METRICS_ENABLED" = 0 || {
  _timetrace "Authenticating for metrics upload"
  # Pre-authenticate for uploading metrics and logs
  "$script_dir"/upload_reproxy_logs.sh --auth-only

  # Generate a uuid for uploading logs and metrics.
  echo "$build_uuid" > "$reproxy_logdir"/build_id
  _timetrace "Authenticating for metrics upload (done)"
}

function shutdown() {
  _timetrace "Shutting down reproxy"
  # b/188923283 -- added --cfg to shut down properly
  shutdown_status=0
  "${bootstrap_env[@]}" \
    "$bootstrap" \
    --shutdown \
    --fast_log_collection \
    --async_reproxy_termination \
    --cfg="$bootstrap_reproxy_cfg" \
    > "$reproxy_logdir"/shutdown.stdout 2>&1 || shutdown_status="$?"
  [[ "$shutdown_status" == 0 && "$verbose" != 1 ]] || {
    cat "$reproxy_logdir"/shutdown.stdout
  }
  _timetrace "Shutting down reproxy (done)"

  cleanup

  [[ "${FX_BUILD_RBE_STATS-NOT_SET}" == "NOT_SET" ]] || {
    "$python" -S "$build_summary_script" "$reproxy_logdir"
  }

  test "$BUILD_METRICS_ENABLED" = 0 || {
    _timetrace "Processing RBE logs and uploading to BigQuery"
    # This script uses the 'bq' CLI tool, which is installed in the
    # same path as `gcloud`.
    # This is experimental and runs a bit noisily for the moment.
    # TODO(https://fxbug.dev/42175720): make this run silently
    cloud_project=fuchsia-engprod-metrics-prod
    dataset=metrics
    "$script_dir"/upload_reproxy_logs.sh \
      --reclient-bindir="$reclient_bindir" \
      --uuid="$build_uuid" \
      --bq-logs-table="$cloud_project:$dataset".rbe_client_command_logs_developer_raw \
      --bq-metrics-table="$cloud_project:$dataset".rbe_client_metrics_developer_raw \
      "$reproxy_logdir"
      # The upload exit status does not propagate from inside a trap call.
    _timetrace "Processing RBE logs and uploading to BigQuery (done)"
  }
}

# EXIT also covers INT
trap shutdown EXIT

# original command is in "$@"
# Do not 'exec' this, so that trap takes effect.
_timetrace "Running wrapped command"
"${rewrapper_env[@]}" "$@"
_timetrace "Running wrapped command (done)"
