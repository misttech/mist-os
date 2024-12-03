# Copyright 2017 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

if [[ -n "${ZSH_VERSION:-}" ]]; then
  devshell_lib_dir=${${(%):-%x}:a:h}
  FUCHSIA_DIR="$(dirname "$(dirname "$(dirname "${devshell_lib_dir}")")")"
else
  # NOTE: Replace use of dirname with BASH substitutions to save 20ms of
  # startup time for all fx commands.
  #
  # Equivalent to "$(dirname ${BASH_SOURCE[0]"}) but 350x faster.
  # Exception: BASH_SOURCE[0] will be the script name when invoked directly
  # from Bash (e.g. cd tools/devshell/lib && bash vars.sh). In this case
  # the substitution will not change anything, so provide fallback case.
  devshell_lib_dir="${BASH_SOURCE[0]%/*}"
  if [[ "${devshell_lib_dir}" == "${BASH_SOURCE[0]}" ]]; then
    devshell_lib_dir="."
  fi
  # Get absolute path.
  devshell_lib_dir="$(cd "${devshell_lib_dir}" >/dev/null 2>&1 && pwd)"
  # Compute absolute path to $devshell_lib_dir/../../..
  FUCHSIA_DIR="${devshell_lib_dir}"
  FUCHSIA_DIR="${FUCHSIA_DIR%/*}"
  FUCHSIA_DIR="${FUCHSIA_DIR%/*}"
  FUCHSIA_DIR="${FUCHSIA_DIR%/*}"
fi

export FUCHSIA_DIR
export FUCHSIA_OUT_DIR="${FUCHSIA_OUT_DIR:-${FUCHSIA_DIR}/out}"
source "${devshell_lib_dir}/platform.sh"
source "${devshell_lib_dir}/fx-cmd-locator.sh"
source "${devshell_lib_dir}/fx-optional-features.sh"
source "${devshell_lib_dir}/generate-ssh-config.sh"
unset devshell_lib_dir

# Subcommands can use this directory to cache artifacts and state that should
# persist between runs. //scripts/fx ensures that it exists.
#
# fx commands that make use of this directory should include the command name
# in the names of any cached artifacts to make naming collisions less likely.
export FX_CACHE_DIR="${FUCHSIA_DIR}/.fx"

# This allows LLVM utilities to perform debuginfod lookups for public artifacts.
# See https://sourceware.org/elfutils/Debuginfod.html.
# TODO(111990): Replace this with a local authenticating proxy to support access
#   control.
public_url="https://storage.googleapis.com/fuchsia-artifacts"
if [[ "$DEBUGINFOD_URLS" != *"$public_url"* ]]; then
  export DEBUGINFOD_URLS="${DEBUGINFOD_URLS:+$DEBUGINFOD_URLS }$public_url"
fi
unset public_url

if [[ "${FUCHSIA_DEVSHELL_VERBOSITY:-0}" -eq 1 ]]; then
  set -x
fi

# If build profiling is enabled, collect system stats during build,
# including CPU, memory, disk I/O...
BUILD_PROFILE_ENABLED=0
readonly fx_build_profile_config="${FUCHSIA_DIR}/.fx-build-profile-config"
if [[ -f "$fx_build_profile_config" ]]
then source "$fx_build_profile_config"
  # This sets BUILD_PROFILE_ENABLED to 0 or 1.
fi

# This wrapper script collects system CPU/mem/IO info while
# another process is running.
readonly profile_wrap="${FUCHSIA_DIR}/build/profile/profile_wrap.sh"

readonly date="$(date +%Y%m%d-%H%M%S)"

readonly jq="$PREBUILT_JQ"

# For commands whose subprocesses may use reclient for RBE, prefix those
# commands conditioned on 'if fx-rbe-enabled' (function).
# This could not be made into a shell-function because it is used
# as both a function and non-built-in command, and functions do not compose
# by prefixing in shell.
RBE_WRAPPER=( "$FUCHSIA_DIR"/build/rbe/fuchsia-reproxy-wrap.sh )
# Propagate tracing option from `fx -x build` to the wrapper script.
# This is less invasive than re-exporting SHELLOPTS.
if [[ -o xtrace ]]; then
  RBE_WRAPPER=( "$SHELL" -x "${RBE_WRAPPER[@]}" )
fi

# Use this to conditionally prefix a command with "${RBE_WRAPPER[@]}".
# NOTE: this function depends on FUCHSIA_BUILD_DIR which is set only after
# initialization.
# The cached version of this function is 'fx-rbe-enabled', below.
function recheck-fx-rbe-enabled {
  # This function is called during tests without a build directory.
  # Returns 1 to indicate that RBE is not enabled.
  fx-build-dir-if-present || return 1

  # This RBE settings file is created at GN gen time.
  local -r rbe_settings_file="${FUCHSIA_BUILD_DIR}/rbe_settings.json"

  # Check to see if the rbe settings indicate that the reproxy wrapper is
  # needed.
  local needs_reproxy=($("$jq" '-r' '.final.needs_reproxy' < "${rbe_settings_file}"))
  if [[ "$needs_reproxy" != "true" ]]; then
    return 1
  fi
}

_fx_rbe_enabled_cache_var=
function fx-rbe-enabled {
  if [[ -z "$_fx_rbe_enabled_cache_var" ]]; then
     recheck-fx-rbe-enabled
     # Cache the return status.
     _fx_rbe_enabled_cache_var="$?"
  fi
  return "$_fx_rbe_enabled_cache_var"
}

# Returns 0 if the build is configured to use authenticated services.
# The value of this function is cached by 'fx-build-needs-auth'.
function recheck-fx-build-needs-auth() {
  # For testing-only, return 1 to indicate that authentication is not needed.
  fx-build-dir-if-present || return 1

  # This RBE settings file is created at GN gen time.
  local -r rbe_settings_file="${FUCHSIA_BUILD_DIR}/rbe_settings.json"

  # Return 0 if authentication is needed for build remote services,
  # otherwise return 1.
  local needs_auth=($("$jq" '-r' '.final.needs_auth' < "${rbe_settings_file}"))
  if [[ "$needs_auth" != "true" ]]; then
    return 1
  fi
}

_fx_build_needs_auth_cache_var=
function fx-build-needs-auth {
  if [[ -z "$_fx_build_needs_auth_cache_var" ]]; then
     recheck-fx-build-needs-auth
     # Cache the return status.
     _fx_build_needs_auth_cache_var="$?"
  fi
  return "$_fx_build_needs_auth_cache_var"
}

# reclient and bazel uses either gcloud or a LOAS credential helper
# to authenticate.
# TODO(b/342026853): if LOAS credentials are unrestricted,
# use the credential helper, otherwise fallback to gcloud.
function fx-check-rbe-setup {
  if ! fx-rbe-enabled ; then return ; fi

  # build/rbe/fuchsia-reproxy-wrap.sh now uses gcert to authenticate
  # See go/rbe/dev/x/reclientoptions#autoauth
  if which gcert > /dev/null ; then return ; fi

  # gcloud is only needed as a backup authentication method
  gcloud="$(which gcloud)" || {
    cat <<EOF

\`gcloud\` command not found (but is needed to authenticate).
\`gcloud\` can be installed from the Cloud SDK:

  http://go/cloud-sdk#installing-and-using-the-cloud-sdk

EOF
  return
  }

# Check authentication first.
# Instruct user to authenticate if needed.
  "$gcloud" auth list 2>&1 | grep -q "$USER@google.com" || {
    cat <<EOF
Did not find credentialed account (\`gcloud auth list\`): $USER@google.com.

To authenticate, run:

  gcloud auth login --update-adc --no-browser

EOF
}

}

# fx-is-stderr-tty exits with success if stderr is a tty.
function fx-is-stderr-tty {
  [[ -t 2 ]]
}

# fx-info prints a line to stderr with a blue INFO: prefix.
function fx-info {
  if fx-is-stderr-tty; then
    echo -e >&2 "\033[1;34mINFO:\033[0m $*"
  else
    echo -e >&2 "INFO: $*"
  fi
}

# fx-warn prints a line to stderr with a yellow WARNING: prefix.
function fx-warn {
  if fx-is-stderr-tty; then
    echo -e >&2 "\033[1;33mWARNING:\033[0m $*"
  else
    echo -e >&2 "WARNING: $*"
  fi
}

# fx-error prints a line to stderr with a red ERROR: prefix.
function fx-error {
  if fx-is-stderr-tty; then
    echo -e >&2 "\033[1;31mERROR:\033[0m $*"
  else
    echo -e >&2 "ERROR: $*"
  fi
}

function fx-gn {
  "${PREBUILT_GN}" "$@"
}

function fx-is-bringup {
  grep '^[^#]*import("//products/bringup.gni")' "${FUCHSIA_BUILD_DIR}/args.gn" >/dev/null 2>&1
}

function _symlink_relative {
  source="$1"
  link="$2"
  if [ -d "$link" ]; then
    # Support ln's behavior passing a directory and reusing the source filename.
    link_dir="$link"
  else
    link_dir="$(dirname "$link")"
  fi
  source_rel="${source#"$link_dir/"}"
  ln -sf "$source_rel" "$link"
}

function _directory_symlink_relative {
  source="$1"
  link="$2"
  # Do _not_ support ln's behavior, remove any existing link if it exists.
  # otherwise a new link will be created into the target directory instead.
  if [ -e "$link" ]; then
    rm "$link"
  fi
  link_dir="$(dirname "$link")"
  source_rel="${source#"$link_dir/"}"
  ln -sf "$source_rel" "$link"
}

function _link_gen_artifacts {
  # These artifacts need to be in the root directory.
  local -r linked_artifacts=("compile_commands.json" "rust-project.json")
  for artifact in "${linked_artifacts[@]}" ; do
    if [[ -f "${FUCHSIA_BUILD_DIR}/$artifact" ]]; then
      _symlink_relative "${FUCHSIA_BUILD_DIR}/$artifact" "${FUCHSIA_DIR}/$artifact"
    fi
  done

  # Create Bazel convenience symlinks. See //build/bazel/README.md
  _directory_symlink_relative \
      "${FUCHSIA_BUILD_DIR}/gen/build/bazel/workspace" \
      "${FUCHSIA_DIR}/bazel-workspace"

  _directory_symlink_relative \
      "${FUCHSIA_BUILD_DIR}/gen/build/bazel/workspace/bazel-bin" \
      "${FUCHSIA_DIR}/bazel-bin"

  _directory_symlink_relative \
      "${FUCHSIA_BUILD_DIR}/gen/build/bazel/workspace/bazel-out" \
      "${FUCHSIA_DIR}/bazel-out"

  _directory_symlink_relative \
      "${FUCHSIA_BUILD_DIR}/gen/build/bazel/output_base/external" \
      "${FUCHSIA_DIR}/bazel-repos"
}

function fx-gen-internal {
  # which of `gn gen` and `gn args` we're doing
  local -r subcommand="$1"
  shift

  # If a user executes gen from a symlinked directory that is not a
  # subdirectory $FUCHSIA_DIR then dotgn search may fail, so execute
  # the gen from the $FUCHSIA_DIR.
  (
    cd "${FUCHSIA_DIR}" && \
    fx-gn "${subcommand}" \
        --fail-on-unused-args \
        --check=system \
        --export-rust-project \
        --ninja-executable="${PREBUILT_NINJA}" \
        --ninja-outputs-file="ninja_outputs.json" \
        "${FUCHSIA_BUILD_DIR}" "$@"
  ) || return $?
  _link_gen_artifacts
}

function fx-gen {
  fx-gen-internal gen "$@"
}

function fx-gn-args {
  fx-gen-internal args "$@"
}

function fx-build-config-load {
  # Paths are relative to FUCHSIA_DIR unless they're absolute paths.
  if [[ "${FUCHSIA_BUILD_DIR:0:1}" != "/" ]]; then
    FUCHSIA_BUILD_DIR="${FUCHSIA_DIR}/${FUCHSIA_BUILD_DIR}"
  fi

  if [[ ! -f "${FUCHSIA_BUILD_DIR}/fx.config" ]]; then
    if [[ ! -f "${FUCHSIA_BUILD_DIR}/args.gn" ]]; then
      fx-error "Build directory missing or removed. (${FUCHSIA_BUILD_DIR})"
      fx-error "run \"fx set\", or specify a build dir with --dir or \"fx use\""
      return 1
    fi

    fx-gen || return 1
  fi

  if ! source "${FUCHSIA_BUILD_DIR}/fx.config"; then
    fx-error "${FUCHSIA_BUILD_DIR}/fx.config caused internal error"
    return 1
  fi

  # The source of `fx.config` will re-set the build dir to relative, so we need
  # to abspath it again.
  if [[ "${FUCHSIA_BUILD_DIR:0:1}" != "/" ]]; then
    FUCHSIA_BUILD_DIR="${FUCHSIA_DIR}/${FUCHSIA_BUILD_DIR}"
  fi


  export FUCHSIA_BUILD_DIR FUCHSIA_ARCH

  if [[ "${HOST_OUT_DIR:0:1}" != "/" ]]; then
    HOST_OUT_DIR="${FUCHSIA_BUILD_DIR}/${HOST_OUT_DIR}"
  fi

  # Set the device specified at the build directory level, if any.
  if [[ -z "${FUCHSIA_NODENAME}" ]]; then
    if [[ -f "${FUCHSIA_BUILD_DIR}.device" ]]; then
      FUCHSIA_NODENAME="$(<"${FUCHSIA_BUILD_DIR}.device")"
      export FUCHSIA_NODENAME_IS_FROM_FILE="true"
    fi
    export FUCHSIA_NODENAME
  fi

  return 0
}

# Sets FUCHSIA_BUILD_DIR once, to an absolute path.
function fx-build-dir-if-present {
  if [[ -n "${FUCHSIA_BUILD_DIR:-}" ]]; then
    # already set by this function earlier
    return 0
  elif [[ -n "${_FX_BUILD_DIR:-}" ]]; then
    export FUCHSIA_BUILD_DIR="${_FX_BUILD_DIR}"
    # This can be set by --dir.
    # Unset to prevent subprocess from acting on it again.
    unset _FX_BUILD_DIR
  else
    if [[ ! -f "${FUCHSIA_DIR}/.fx-build-dir" ]]; then
      return 1
    fi

    # .fx-build-dir contains $FUCHSIA_BUILD_DIR
    FUCHSIA_BUILD_DIR="$(<"${FUCHSIA_DIR}/.fx-build-dir")"
    if [[ -z "${FUCHSIA_BUILD_DIR}" ]]; then
      return 1
    fi
    # Paths are relative to FUCHSIA_DIR unless they're absolute paths.
    if [[ "${FUCHSIA_BUILD_DIR:0:1}" != "/" ]]; then
      FUCHSIA_BUILD_DIR="${FUCHSIA_DIR}/${FUCHSIA_BUILD_DIR}"
    fi
  fi
  return 0
}

function fx-config-read {
  if ! fx-build-dir-if-present; then
    fx-error "No build directory found."
    fx-error "Run \"fx set\" to create a new build directory, or specify one with --dir"
    exit 1
  fi

  fx-build-config-load || exit $?

  _FX_LOCK_FILE="${FUCHSIA_BUILD_DIR}.build_lock"
}

function fx-change-build-dir {
  local build_dir="$1"

  local -r tempfile="$(mktemp)"
  echo "${build_dir}" > "${tempfile}"
  mv -f "${tempfile}" "${FUCHSIA_DIR}/.fx-build-dir"

  # Now update the environment and root-symlinked build artifacts to reflect
  # the change.
  fx-config-read

  _link_gen_artifacts

}

function ffx-default-repository-name {
    # Use the build directory's name by default. Note that package URLs are not
    # allowed to have underscores, so replace them with hyphens.
    basename "${FUCHSIA_BUILD_DIR}" | tr '_' '-'
}

# Runs a jq command against an existing file that will edit it, taking care
# of piping output to a tempfile and replacing it if jq succeeds.
# `_jq_edit <jq args> path/to/file.json`
#
# Note that the last argument is expected to be the filename being edited, to
# match the normal argument order of jq, but unlike with jq it's required.
function _jq_edit {
  # Take the last argument as the path to the edited file.
  local json_file="${@: -1}"

  local -r tempfile="$(mktemp)"
  if fx-command-run jq "$@" > "${tempfile}" ; then
    mv -f "${tempfile}" "${json_file}"
    return 0
  else
    return $?
  fi
}

# Set a configuration value in a build config file:
# `json-config-set path/to/file.json path.to.value "value"`
function json-config-set {
  local json_file="$1"
  local path="$2"
  local value="$3"

  # There needs to be a file there in the first place, so if there isn't one
  # create one with an empty object for jq to update.
  if ! [[ -f "${json_file}" ]] ; then
    echo "{}" > "${json_file}"
  fi

  _jq_edit -e -cS --arg value "${value}" ".${path} = \$value" "${json_file}"
  return $?
}

# Remove a configuration value in a build config file:
# `json-config-del path/to/file.json path.to.value`
#
# Will return a non-zero status code if the file or value did not already
# exist.
function json-config-del {
  local json_file="$1"
  local path="$2"

  if [[ -f "${json_file}" ]] ; then
    # check the path exists, delete it if so, exit with error code otherwise.
    _jq_edit -e -cS "if .${path} then del(.${path}) else empty | halt_error(1) end" "${json_file}"
    return $?
  else
    return 1
  fi
}

# Get a configuration value in a build config file:
# `json-config-get path/to/file.json path.to.value`
#
# Returns 1 and prints an empty line if the file does not exist or
# the path given was not already set.
function json-config-get {
  local json_file="$1"
  local path="$2"

  if ! [[ -f "${json_file}" ]] ; then
    echo
    return 1
  else
    fx-command-run jq -e -r ".${path} | values" "${json_file}"
  fi
}

function get-device-raw {
  fx-config-read
  local device=""
  device="$(ffx target default get)"

  if ! is-valid-device "${device}"; then
    fx-error "Invalid device name or address: '${device}'. Some valid examples are:
      strut-wind-ahead-turf, 192.168.3.1:8022, [fe80::7:8%eth0], [fe80::7:8%eth0]:5222, [::1]:22"
    exit 1
  fi
  echo "${device}"
}

function is-valid-device {
  local device="$1"
  if [[ -n "${device}" ]] \
      && ! _looks_like_ipv4 "${device}" \
      && ! _looks_like_ipv6 "${device}" \
      && ! _looks_like_hostname "${device}"; then
    return 1
  fi
}

# Shared among a few subcommands to configure and identify a remote forward
# target for a device.
export _FX_REMOTE_WORKFLOW_DEVICE_ADDR='[::1]:8022'

function is-remote-workflow-device {
  [[ $(get-device-raw 2>/dev/null) == "${_FX_REMOTE_WORKFLOW_DEVICE_ADDR}" ]]
}

# fx-export-device-address is "public API" to commands that wish to
# have the exported variables set.
function fx-export-device-address {
  FX_DEVICE_NAME="$(get-device-name)"
  FX_DEVICE_ADDR="$(get-fuchsia-device-addr)"
  FX_SSH_ADDR="$(get-device-addr-resource)"
  FX_SSH_PORT="$(get-device-ssh-port)"
  export FX_DEVICE_NAME FX_DEVICE_ADDR FX_SSH_ADDR FX_SSH_PORT
}

function get-device-ssh-port {
  local device
  device="$(get-device-raw)" || exit $?
  local port=""
  # extract port, if present
  if [[ "${device}" =~ :([0-9]+)$ ]]; then
    port="${BASH_REMATCH[1]}"
  fi
  echo "${port}"
}

function get-device-name {
  local device
  device="$(get-device-raw)" || exit $?
  # remove ssh port if present
  if _looks_like_hostname "${device}" || _looks_like_ipv4 "${device}"; then
    if [[ "${device}" =~ ^(.*):[0-9]{1,5}$ ]]; then
      device="${BASH_REMATCH[1]}"
    fi
  elif _looks_like_ipv6 "${device}"; then
    # parse the address into parts.
    local expression='^\[([0-9a-fA-F:]+(%[0-9a-zA-Z-]{1,})?)\](:[0-9]{1,5})?$'
    if ! [[ "$device" =~ ${expression} ]]; then
      # try again but wrap the arg in "[]"
      local wrapped="[${device}]"
      if ! [[ "$device" =~ ${expression} ]]; then
        echo "$device"
        return
      fi
    fi
    device="[${BASH_REMATCH[1]}]"

  fi
  echo "${device}"
}

function _looks_like_hostname {
  [[ "$1" =~ ^([a-z0-9][.a-z0-9-]*)?(:[0-9]{1,5})?$ ]] || return 1
}

function _looks_like_ipv4 {
  [[ "$1" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}(:[0-9]{1,5})?$ ]] || return 1
}

function _looks_like_ipv6 {
  local expression
  expression='^\[([0-9a-fA-F:]+(%[0-9a-zA-Z-]{1,})?)\](:[0-9]{1,5})?$'
  if ! [[ "$1" =~ ${expression} ]]; then
    # try again but wrap the arg in "[]"
    local wrapped
    wrapped="[${1}]"
    [[ "${wrapped}" =~ ${expression} ]] || return 1
    # Okay check that the address is not just a bare port
    ! [[ "$1" =~ ^:[0-9a-fA-F]+$ ]] || return 1
    # Okay check that the address is not just a host name
    ! [[ "$1" =~ ^[0-9a-fA-F\.]+$ ]] || return 1
  fi
  local colons="${BASH_REMATCH[1]//[^:]}"
  # validate that there are no more than 7 colons
  [[ "${#colons}" -le 7 ]] || return 1
}

function _print_ssh_warning {
  fx-warn "Cannot load device SSH credentials. $*"
  fx-warn "Run 'tools/ssh-keys/gen-ssh-keys.sh' to regenerate."
}

# Checks if default SSH keys are missing.
#
# The argument specifies which line of the manifest to retrieve and verify for
# existence.
#
# "key": The SSH identity file (private key). Append ".pub" for the
#   corresponding public key.
# "auth": The authorized_keys file.
function _get-ssh-key {
  local -r _SSH_MANIFEST="${FUCHSIA_DIR}/.fx-ssh-path"

  local filepath
  local -r which="$1"
  if [[ ! "${which}" =~ ^(key|auth)$ ]]; then
    fx-error "_get-ssh-key: invalid argument '$1'. Must be either 'key' or 'auth'"
    exit 1
  fi

  if [[ ! -f "${_SSH_MANIFEST}" ]]; then
    _print_ssh_warning "File not found: ${_SSH_MANIFEST}."
    return 1
  fi

  # Set -r flag to avoid interpreting backslashes as escape characters, e.g. it
  # won't convert "\n" to a newline character.
  { read -r privkey && read -r authkey; } < "${_SSH_MANIFEST}"

  if [[ -z $privkey || -z $authkey ]]; then
    _print_ssh_warning "Manifest file ${_SSH_MANIFEST} is malformed."
    return 1
  fi

  if [[ $which == "auth" ]]; then
    filepath="${authkey}"
  elif [[ $which == "key" ]]; then
    filepath="${privkey}"
  fi

  echo "${filepath}"

  if [[ ! -f "${filepath}" ]]; then
    _print_ssh_warning "File not found: ${filepath}."
    return 1
  fi

  return 0
}

# Invoke the ffx tool. This function invokes ffx via the tools/devshell/ffx
# wrapper that passes configuration information such as default target and build
# directory locations to the ffx tool as needed to provide a seamless fx/ffx
# integration.
function ffx {
  fx-command-run ffx --config fuchsia.analytics.ffx_invoker=fx "$@"
}

# Prints path to the default SSH key. These credentials are created
# and configured via ffx.
#
# The corresponding public key is stored in "$(get-ssh-privkey).pub".
function get-ssh-privkey {
  init="$(fx-command-run ffx --config fuchsia.analytics.ffx_invoker=fx config check-ssh-keys)"
  RESULT=$?
  if [ $RESULT -ne 0 ]; then
    fx-error "$init"
    return 1
  fi
  val="$(fx-command-run ffx --config fuchsia.analytics.ffx_invoker=fx config get --process file ssh.priv)"
  temp="${val%\"}"
  authkeys="${temp#\"}"
  echo "${authkeys}"
}

# Prints path to the default authorized_keys to include on Fuchsia devices.
function get-ssh-authkeys {
  init="$(fx-command-run ffx --config fuchsia.analytics.ffx_invoker=fx config check-ssh-keys)"
  RESULT=$?
  if [ $RESULT -ne 0 ]; then
    fx-error "$init"
    return 1
  fi
  val="$(fx-command-run ffx --config fuchsia.analytics.ffx_invoker=fx config get --process file ssh.pub)"
  temp="${val%\"}"
  authkeys="${temp#\"}"
  echo "${authkeys}"
}

# Checks the ssh_config file exists and references the private key, otherwise
# (re)creates it
function check-ssh-config {
  privkey="$(get-ssh-privkey)"
  conffile="${FUCHSIA_BUILD_DIR}/ssh-keys/ssh_config"
  if [[ ! -f "${conffile}" ]] || ! grep -q "IdentityFile\s*$privkey" "$conffile"; then
    generate-ssh-config "$privkey" "$conffile"
    if [[ $? -ne 0 || ! -f "${conffile}" ]] || ! grep -q "IdentityFile\s*$privkey" "$conffile"; then
      fx-error "Unexpected error, cannot generate ssh_config: ${conffile}"
      exit 1
    fi
  fi
}

function fx-target-finder-resolve {
  if [[ $# -ne 1 ]]; then
    fx-error "Invalid arguments to fx-target-finder-resolve: [$*]"
    return 1
  fi
  ffx target list --format a "$1"
}

function fx-target-finder-list {
  ffx target list --format a
}

function fx-target-finder-info {
  ffx target list --format s
}

function fx-target-ssh-address {
  ffx target get-ssh-address
}

function multi-device-fail {
  local output devices
  fx-error "Multiple devices found."
  fx-error "Please specify one of the following devices using either \`fx -d <device-name>\` or \`fx set-device <device-name>\`."
  devices="$(fx-target-finder-info)" || {
    code=$?
    fx-error "Device discovery failed with status: $code"
    exit $code
  }
  while IFS="" read -r line; do
    fx-error "\t${line}"
  done < <(printf '%s\n' "${devices}")
  exit 1
}

function get-fuchsia-device-addr {
  fx-config-read
  local device
  device="$(get-device-name)" || exit $?

  # Treat IPv4 addresses in the device name as an already resolved
  # device address.
  if _looks_like_ipv4 "${device}"; then
    echo "${device}"
    return
  fi
  if _looks_like_ipv6 "${device}"; then
    # remove brackets
    device="${device%]}"
    device="${device#[}"
    echo "${device}"
    return
  fi

  local output devices
  case "$device" in
    "")
        output="$(fx-target-finder-list)" || {
          code=$?
          fx-error "Device discovery failed with status: $code"
          exit $code
        }
        if [[ "$(echo "${output}" | wc -l)" -gt "1" ]]; then
          multi-device-fail
        fi
        echo "${output}" ;;
     *) fx-target-finder-resolve "$device" ;;
  esac
}

function get-fuchsia-device-port {
  fx-config-read
  local port
  port="$(get-device-ssh-port)" || exit $?

  if [[ -z "${port}" ]]; then
    local device
    device="$(fx-target-ssh-address)" || {
      code=$?
      fx-error "Device discovery failed with status: ${code}"
      exit ${code}
    }
    if [[ "$(echo "${device}" | wc -l)" -gt "1" ]]; then
      multi-device-fail
    fi
    if [[ "${device}" =~ :([0-9]+)$ ]]; then
      port="${BASH_REMATCH[1]}"
    fi
  fi
  echo "${port}"
}

# get-device-addr-resource returns an address that is properly encased
# for resource addressing for tools that expect that. In practical
# terms this just means encasing the address in square brackets if it
# is an ipv6 address. Note: this is not URL safe as-is, use the -url
# variant instead for URL formulation.
function get-device-addr-resource {
  local addr
  addr="$(get-fuchsia-device-addr)" || exit $?
  if _looks_like_ipv4 "${addr}"; then
    echo "${addr}"
    return 0
  fi

  echo "[${addr}]"
}

function get-device-addr-url {
  get-device-addr-resource | sed 's#%#%25#'
}

function fx-command-run {
  local -r command_name="$1"
  local command_path
  # Use an array because the command may be multiple elements, if it comes from
  # a .fx metadata file.
  command_path=( $(find_executable "${command_name}") )

  if [[ $? -ne 0 || ! -x "${command_path[0]}" ]]; then
    fx-error "Unknown command ${command_name}"
    exit 1
  fi

  shift
  env FX_CALLER="$0" "${command_path[@]}" "$@"
}

function fx-command-exec {
  local -r command_name="$1"
  local command_path
  # Use an array because the command may be multiple elements, if it comes from
  # a .fx metadata file.
  command_path=( $(find_executable "${command_name}") )

  if [[ $? -ne 0 || ! -x "${command_path[0]}" ]]; then
    fx-error "Unknown command ${command_name}"
    exit 1
  fi

  shift
  exec env FX_CALLER="$0" "${command_path[@]}" "$@"
}

function fx-print-command-help {
  local command_path="$1"
  if grep '^## ' "$command_path" > /dev/null; then
    sed -n -e 's/^## //p' -e 's/^##$//p' < "$command_path"
  else
    local -r command_name=$(basename "$command_path" ".fx")
    echo "No help found. Try \`fx $command_name -h\`"
  fi
}

function fx-command-help {
  fx-print-command-help "$0"
  echo -e "\nFor global options, try \`fx help\`."
}


# This function massages arguments to an fx subcommand so that a single
# argument `--switch=value` becomes two arguments `--switch` `value`.
# This lets each subcommand's main function use simpler argument parsing
# while still supporting the preferred `--switch=value` syntax.  It also
# handles the `--help` argument by redirecting to what `fx help command`
# would do.  Because of the complexities of shell quoting and function
# semantics, the only way for this function to yield its results
# reasonably is via a global variable.  FX_ARGV is an array of the
# results.  The standard boilerplate for using this looks like:
#   function main {
#     fx-standard-switches "$@"
#     set -- "${FX_ARGV[@]}"
#     ...
#     }
# Arguments following a `--` are also added to FX_ARGV but not split, as they
# should usually be forwarded as-is to subprocesses.
function fx-standard-switches {
  # In bash 4, this can be `declare -a -g FX_ARGV=()` to be explicit
  # about setting a global array.  But bash 3 (shipped on macOS) does
  # not support the `-g` flag to `declare`.
  FX_ARGV=()
  while [[ $# -gt 0 ]]; do
    if [[ "$1" = "--help" || "$1" = "-h" ]]; then
      fx-print-command-help "$0"
      # Exit rather than return, so we bail out of the whole command early.
      exit 0
    elif [[ "$1" == --*=* ]]; then
      # Turn --switch=value into --switch value.
      FX_ARGV+=("${1%%=*}" "${1#*=}")
    elif [[ "$1" == "--" ]]; then
      # Do not parse remaining parameters after --
      FX_ARGV+=("$@")
      return
    else
      FX_ARGV+=("$1")
    fi
    shift
  done
}

function fx-uuid {
  # Emit a uuid string, same as the `uuidgen` tool.
  # Using Python avoids requiring a separate tool.
  "${PREBUILT_PYTHON3}" -S -c 'import uuid; print(uuid.uuid4())'
}

function fx-choose-build-concurrency {
  # If any remote execution is enabled (e.g. via RBE),
  # allow ninja to launch many more concurrent actions than what local
  # resources can support.
  if fx-rbe-enabled ; then
    # The recommendation from the Goma team is to use 10*cpu-count for C++.
    local cpus
    cpus="$(fx-cpu-count)"
    echo $((cpus * 10))
  else
    fx-cpu-count
  fi
}

function fx-cpu-count {
  local -r cpu_count=$(getconf _NPROCESSORS_ONLN)
  echo "$cpu_count"
}


# Use a lock file around a command if possible.
# Print a message if the lock isn't immediately entered,
# and block until it is.
function fx-try-locked {
  if [[ -z "${_FX_LOCK_FILE}" ]]; then
    fx-error "fx internal error: attempt to run locked command before fx-config-read"
    exit 1
  fi
  if ! command -v shlock >/dev/null; then
    # Can't lock! Fall back to unlocked operation.
    fx-exit-on-failure "$@"
  elif shlock -f "${_FX_LOCK_FILE}" -p $$; then
    # This will cause a deadlock if any subcommand calls back to fx build,
    # because shlock isn't reentrant by forked processes.
    fx-cmd-locked "$@"
  else
    echo "Locked by ${_FX_LOCK_FILE}..."
    while ! shlock -f "${_FX_LOCK_FILE}" -p $$; do sleep .1; done
    fx-cmd-locked "$@"
  fi
}

function fx-cmd-locked {
  if [[ -z "${_FX_LOCK_FILE}" ]]; then
    fx-error "fx internal error: attempt to run locked command before fx-config-read"
    exit 1
  fi
  # Exit trap to clean up lock file. Intentionally use the current value of
  # $_FX_LOCK_FILE rather than the value at the time that trap executes to
  # ensure we delete the original file even if the value of $_FX_LOCK_FILE
  # changes for whatever reason.
  trap "[[ -n \"${_FX_LOCK_FILE}\" ]] && rm -f \"${_FX_LOCK_FILE}\"" EXIT
  fx-exit-on-failure "$@"
}

function fx-exit-on-failure {
  "$@" || exit $?
}

# Massage a ninja command line to add default -j and/or -l switches.
# Arguments:
#    print_full_cmd   if true, prints the full ninja command line before
#                     executing it
#    ninja command    the ninja command itself. This can be used both to run
#                     ninja directly or to run a wrapper script around ninja.
function fx-run-ninja {
  # Separate the command from the arguments so we can prepend default -j/-l
  # switch arguments.  They need to come before the user's arguments in case
  # those include -- or something else that makes following arguments not be
  # handled as normal switches.
  local print_full_cmd="$1"
  shift
  local cmd="$1"
  shift

  local args=()
  local full_cmdline
  local cpu_load
  local concurrency
  local have_load=false
  local have_jobs=false
  while [[ $# -gt 0 ]]; do
    case "$1" in
    -l)
      have_load=true
      cpu_load="$2"
      ;;
    -j)
      have_jobs=true
      concurrency="$2"
      ;;
    -l*)
      have_load=true
      cpu_load="${1#-l}"
      ;;
    -j*)
      have_jobs=true
      concurrency="${1#-j}"
      ;;
    esac
    args+=("$1")
    shift
  done

  if ! "$have_load"; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
      # Load level on Darwin is quite different from that of Linux, wherein a
      # load level of 1 per CPU is not necessarily a prohibitive load level. An
      # unscientific study of build side effects suggests that cpus*20 is a
      # reasonable value to prevent catastrophic load (i.e. user can not kill
      # the build, can not lock the screen, etc).
      local cpus
      cpus="$(fx-cpu-count)"
      cpu_load=$((cpus * 20))
      args=("-l" "${cpu_load}" "${args[@]}")
    fi
  elif [[ -z "${cpu_load}" ]]; then
    echo "ERROR: Missing cpu load (-l) argument."
    exit 1
  fi

  if ! "$have_jobs"; then
    concurrency="$(fx-choose-build-concurrency)"
    # macOS in particular has a low default for number of open file descriptors
    # per process, which is prohibitive for higher job counts. Here we raise
    # the number of allowed file descriptors per process if it appears to be
    # low in order to avoid failures due to the limit. See `getrlimit(2)` for
    # more information.
    local min_limit=$((concurrency * 2))
    if [[ $(ulimit -n) -lt "${min_limit}" ]]; then
      ulimit -n "${min_limit}"
    fi
    args=("-j" "${concurrency}" "${args[@]}")
  elif [[ -z "${concurrency}" ]]; then
    echo "ERROR: Missing job count (-j) argument."
    exit 1
  fi

  # Check for a bad element in $PATH.
  # We build tools in the build, such as touch(1), targeting Fuchsia. Those
  # tools end up in the root of the build directory, which is also $PWD for
  # tool invocations. As we don't have hermetic locations for all of these
  # tools, when a user has an empty/pwd path component in their $PATH,
  # the Fuchsia target tool will be invoked, and will fail.
  # Implementation detail: Normally you would split path with IFS or a similar
  # strategy, but catching the case where the first or last components are
  # empty can be tricky in that case, so the pattern match strategy here covers
  # the cases more easily. We check for three cases: empty prefix, empty suffix
  # and empty inner.
  case "${PATH}" in
  :*|*:|*::*)
    fx-error "Your \$PATH contains an empty element that will result in build failure."
    fx-error "Remove the empty element from \$PATH and try again."
    echo "${PATH}" | grep --color -E '^:|::|:$' >&2
    exit 1
  ;;
  .:*|*:.|*:.:*)
    fx-error "Your \$PATH contains the working directory ('.') that will result in build failure."
    fx-error "Remove the '.' element from \$PATH and try again."
    echo "${PATH}" | grep --color -E '^.:|:.:|:.$' >&2
    exit 1
  ;;
  esac


  # TERM is passed for the pretty ninja UI
  # PATH is passed through.  The ninja actions should invoke tools without
  # relying on PATH.
  # TMPDIR was passed for Goma on macOS, but it might have other uses.
  # NINJA_STATUS, NINJA_STATUS_MAX_COMMANDS and NINJA_STATUS_REFRESH_MILLIS
  # are passed to control Ninja progress status.
  #
  # rbe_wrapper is used to auto-start/stop a (reclient) proxy process for the
  # duration of the build, so that RBE-enabled build actions can operate
  # through the proxy.
  #
  local -r build_uuid="$(fx-uuid)"
  local -a user_rbe_env=()

  local -a rbe_wrapper_loas_args=()
  if fx-build-needs-auth
  then
    local loas_type
    # TODO(b/342026853): automatic use of gcert for authentication in bazel
    # is still experimental, and is opt-in with FX_BUILD_AUTO_AUTH=1.
    # gcert authentication for reclient already works.
    local -r loas_type_detected="$(fx-command-run rbe _check_loas_type)"
    local -r loas_type_for_reclient="$loas_type_detected"
    local loas_type_for_bazel
    if [[ "$FX_BUILD_AUTO_AUTH" == 1 ]]
    then loas_type_for_bazel="$loas_type_detected"
    else loas_type_for_bazel="restricted"
    fi
    rbe_wrapper_loas_args+=( --loas-type="$loas_type_for_reclient" )
    user_rbe_env+=(
      # Automatic auth with gcert (from re-client bootstrap) needs $USER.
      "USER=${USER}"
      "FX_BUILD_LOAS_TYPE=$loas_type_for_bazel"
      # A few tools need credentials for authentication, like remotetool.
      # Explicitly set this variable without forwarding $HOME.
      # User-overridable.
      "GOOGLE_APPLICATION_CREDENTIALS=${GOOGLE_APPLICATION_CREDENTIALS:-$HOME/.config/gcloud/application_default_credentials.json}"
      # For bazel subinvocations to be able to authenticate with gcert,
      # need to forward the authentication socket (used by gnubby).
      "SSH_AUTH_SOCK=${SSH_AUTH_SOCK}"
    )
  fi

  local -a rbe_wrapper=()
  if fx-rbe-enabled
  then
    # Move the reproxy logs outside of $FUCHSIA_BUILD_DIR so they do not get cleaned,
    # but under 'out' so it does not pollute the source root.
    # `fx rbe cleanlogs` will remove all of the accumulated reproxy logs.
    # Choose a unique reproxy log dir based on basename of $FUCHSIA_BUILD_DIR.
    local -r _build_dir_base="${FUCHSIA_BUILD_DIR##*/}"  # basename

    # LINT.IfChange(reproxy_log_dirs)
    local -r _logs_root="$FUCHSIA_DIR/out/.reproxy_logs/$_build_dir_base"
    # LINT.ThenChange(/tools/devshell/rbe:reproxy_log_dirs)
    mkdir -p "$_logs_root"
    # 'mktemp -p' still yields to TMPDIR in the environment (bug?),
    # so override TMPDIR instead.
    local -r reproxy_logdir="$(env TMPDIR="$_logs_root" mktemp -d -t "reproxy.$date.XXXX")"
    local -r _log_base="${reproxy_logdir##*/}"  # basename

    # reproxy wants temporary space on the same physical device where the build happens.
    # Re-use the randomly generated dir name in a custom tempdir.
    local -r reproxy_tmpdir="$FUCHSIA_BUILD_DIR/.reproxy_tmpdirs/$_log_base"
    mkdir -p "$reproxy_tmpdir"

    # Honor additional cfg files from the current build dir.
    local -r rbe_config_json="$FUCHSIA_BUILD_DIR/rbe_config.json"
    local proxy_cfg_args=()
    if [[ -r "$rbe_config_json" ]]
    then
      all_proxy_cfgs=($("$jq" '.[] | .path' < "$rbe_config_json" | sed -e 's|"\(.*\)"|\1|'))
      # Adjust paths to be absolute.
      for f in "${all_proxy_cfgs[@]}"
      do proxy_cfg_args+=(--cfg "$FUCHSIA_BUILD_DIR/$f")  # cumulative, repeatable
      done
    fi

    rbe_wrapper=(
      env
      "${RBE_WRAPPER[@]}"
      --logdir "$reproxy_logdir"
      --tmpdir "$reproxy_tmpdir"
      "${proxy_cfg_args[@]}"
      "${rbe_wrapper_loas_args[@]}"
      --
    )
    [[ "${USER-NOT_SET}" != "NOT_SET" ]] || {
      echo "Error: USER is not set"
      exit 1
    }
    user_rbe_env+=(
      # Honor environment variable to disable RBE build metrics.
      "FX_REMOTE_BUILD_METRICS=${FX_REMOTE_BUILD_METRICS}"
    )
  fi

  envs=(
    "FX_BUILD_UUID=$build_uuid"
    "${user_rbe_env[@]}"
    "TERM=${TERM}"
    "PATH=${PATH}"
    # By default, also show the number of actively running actions.
    "NINJA_STATUS=${NINJA_STATUS:-"[%f/%t][%p/%w](%r) "}"
    # By default, print the 4 oldest commands that are still running.
    "NINJA_STATUS_MAX_COMMANDS=${NINJA_STATUS_MAX_COMMANDS:-4}"
    "NINJA_STATUS_REFRESH_MILLIS=${NINJA_STATUS_REFRESH_MILLIS:-100}"
    "NINJA_PERSISTENT_MODE=${NINJA_PERSISTENT_MODE:-0}"
    # Forward the following only if the environment already sets them:
    ${MAKEFLAGS+"MAKEFLAGS=${MAKEFLAGS}"}
    ${FUCHSIA_BAZEL_DISK_CACHE+"FUCHSIA_BAZEL_DISK_CACHE=${FUCHSIA_BAZEL_DISK_CACHE}"}
    ${FUCHSIA_DEBUG_BAZEL_SANDBOX+"FUCHSIA_DEBUG_BAZEL_SANDBOX=${FUCHSIA_DEBUG_BAZEL_SANDBOX}"}
    ${NINJA_PERSISTENT_TIMEOUT_SECONDS+"NINJA_PERSISTENT_TIMEOUT_SECONDS=$NINJA_PERSISTENT_TIMEOUT_SECONDS"}
    ${NINJA_PERSISTENT_LOG_FILE+"NINJA_PERSISTENT_LOG_FILE=$NINJA_PERSISTENT_LOG_FILE"}
    ${TMPDIR+"TMPDIR=$TMPDIR"}
    ${CLICOLOR_FORCE+"CLICOLOR_FORCE=$CLICOLOR_FORCE"}
    ${FX_BUILD_RBE_STATS+"FX_BUILD_RBE_STATS=$FX_BUILD_RBE_STATS"}
    ${FX_BUILD_AUTO_AUTH+"FX_BUILD_AUTO_AUTH=$FX_BUILD_AUTO_AUTH"}
  )

  if [[ "${have_jobs}" ]]; then
    # Pass any _explicit_ job count provided by the user to the Bazel
    # launcher script through an environment variable.
    # See https://fxbug.dev/351623259
    envs+=("FUCHSIA_BAZEL_JOB_COUNT=${concurrency}")
  fi

  local profile_wrapper=()
  if [[ "$BUILD_PROFILE_ENABLED" == 1 ]]
  then
    # Collect system profile data while build is running.
    # Note: this profile dir will get cleaned by 'fx clean'
    local profile_dir="${FUCHSIA_BUILD_DIR}/.build_profile"
    mkdir -p "$profile_dir"
    local vmstat_log="$(env TMPDIR="$profile_dir" mktemp -t "vmstat.$date.XXXX.log")"
    local ifconfig_log="$(env TMPDIR="$profile_dir" mktemp -t "ifconfig.$date.XXXX.log")"
    profile_wrapper=(
      "$profile_wrap"
      --vmstat-log "$vmstat_log"
      --ifconfig-log "$ifconfig_log"
      -n 2
      --
    )
  fi

  full_cmdline=(
    env -i "${envs[@]}"
    "${profile_wrapper[@]}"
    "${rbe_wrapper[@]}"
    "$cmd"
    "${args[@]}"
  )

  if [[ "${print_full_cmd}" = true ]]; then
    echo "${full_cmdline[@]}"
    echo
  fi
  fx-try-locked "${full_cmdline[@]}"
}

function fx-get-image {
  fx-command-run list-build-artifacts --name "$1" --type "$2" --expect-one images
}

function fx-get-zbi {
  fx-get-image "$1" zbi
}

function fx-get-qemu-kernel {
  fx-get-image qemu-kernel kernel
}

function fx-zbi {
  "${FUCHSIA_BUILD_DIR}/$(fx-command-run list-build-artifacts --name zbi --expect-one tools)" --compressed="$FUCHSIA_ZBI_COMPRESSION" "$@"
}

function fx-zbi-default-compression {
  "${FUCHSIA_BUILD_DIR}/$(fx-command-run list-build-artifacts --name zbi --expect-one tools)" "$@"
}
