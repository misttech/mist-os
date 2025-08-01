#!/usr/bin/env bash
#
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can bt
# found in the LICENSE file.
#
# SYNOPSIS
#   Builds a Gerrit MCP server and configures its Gemini extension.
#
# DESCRIPTION
#   This script automates the process of building a local Gerrit MCP
#    server and generating the necessary `gemini-extension.json`
#    configuration file that points to it.
#
#   It performs the following steps:
#   1. Validates that dependencies (jq) are installed.
#   2. Runs the build script for the specified Gerrit MCP server.
#   3. Generates or updates the `gemini-extension.json` with the correct
#      paths to the server's Python executable, main script, and context file.
#
# USAGE
#   ./build.sh [--mcp-server-path PATH]
#
# DEPENDENCIES
#   - jq

# --- Strict Mode ---
set -euo pipefail

# --- Constants ---
readonly SCRIPT_NAME="$(basename "${BASH_SOURCE[0]}")"
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly EXTENSION_DIR="${SCRIPT_DIR}/extensions/gerrit"
readonly GEMINI_EXTENSION_JSON_PATH="${EXTENSION_DIR}/gemini-extension.json"

# --- Functions ---

# A cross-platform realpath function for existing paths.
# Uses realpath if available, otherwise falls back to a shell implementation.
_realpath() {
  if command -v realpath &>/dev/null; then
    realpath "$1"
  else
    # Fallback for systems without realpath (eg: macOS) for EXISTING paths
    if [[ -d "$1" ]]; then
      (cd "$1" && pwd)
    else
      local dir
      dir=$(dirname -- "$1")
      local base
      base=$(basename -- "$1")
      (cd "${dir}" && printf '%s/%s\n' "$(pwd)" "${base}")
    fi
  fi
}

# Prints a message to stderr.
# Usage: log::info "Doing something..."
log::info() {
  printf >&2 '[%s] INFO: %s\n' "${SCRIPT_NAME}" "$*"
}

# Prints an error message to stderr and exits.
# Usage: log::error "Error message" [exit_code]
log::error() {
  local msg="${1}"
  local code="${2:-1}" # Default exit code is 1
  printf >&2 '[%s] ERROR: %s\n' "${SCRIPT_NAME}" "${msg}"
  exit "${code}"
}

# Prints the script usage information.
usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Configures and builds the Gerrit MCP server.

If --install-path is not provided, the script runs interactively,
defaulting to installing the repo in \$HOME/mcp-servers.

The --install-path should point to the directory where the 'mcp-servers'
repository will be cloned or updated.

OPTIONS:
  --install-path PATH      Path for the 'mcp-servers' repository.
  --update-only [PATH]     Only update the repository at the given or default
                           path and regenerate the config. Does not run the build.
                           (Default: \$HOME/mcp-servers/gerrit)
  -h, --help               Show this help message.
EOF
}

# Validates that required dependencies are installed.
validate_dependencies() {
  log::info "Validating dependencies..."
  if ! command -v jq &>/dev/null; then
    log::error "Missing required command: 'jq'. Please install it to continue."
  fi
}

# Builds the Gerrit MCP server.
# Arguments:
#   $1: The absolute path to the Gerrit MCP server directory.
build_gerrit() {
  local mcp_path="$1"
  log::info "Using Gerrit MCP server path: ${mcp_path}"

  if [[ ! -d "${mcp_path}" ]]; then
    log::error "Directory not found at ${mcp_path}"
  fi

  local build_script_path="${mcp_path}/build-gerrit.sh"
  if [[ ! -f "${build_script_path}" ]]; then
    log::error "Build script not found at ${build_script_path}"
  fi

  log::info "Running the build script..."
  (cd "${mcp_path}" && bash ./build-gerrit.sh)
  log::info "The mcp-server repository was installed to: $(dirname "${mcp_path}")"
  log::info "Build script finished successfully."
}

# Pulls the latest changes from the main branch of a Git repository.
# Arguments:
#   $1: The absolute path to the Git repository.
update_repo() {
  local repo_path="$1"
  log::info "Updating repository at ${repo_path}..."

  if [[ ! -d "${repo_path}/.git" ]]; then
    log::error "Not a Git repository: ${repo_path}"
  fi

  (
    cd "${repo_path}"
    if git pull origin main; then
      log::info "Repository updated successfully."
    else
      log::error "Failed to update the repository."
    fi
  )
}

# Generates or updates the Gemini extension configuration file. This function
# is idempotent and will create the correct file from scratch.
# Arguments:
#   $1: The absolute path to the Gerrit MCP server directory.
generate_config_file() {
  local mcp_path="$1"
  log::info "Generating configuration in ${GEMINI_EXTENSION_JSON_PATH}"

  mkdir -p "${EXTENSION_DIR}"
  rm -f "${EXTENSION_DIR}/gemini-extension-old.json"

  log::info "Creating configuration from built-in template."

  local tmp_json_path="${GEMINI_EXTENSION_JSON_PATH}.tmp"
  jq -n \
    --arg cmd "${mcp_path}/.venv/bin/python" \
    --arg main_py "${mcp_path}/gerrit_mcp_server/main.py" \
    --arg python_path "${mcp_path}" \
    --arg context_file "${mcp_path}/README.md" \
    '{
      "name": "gerrit",
      "version": "1.0.0",
      "description": "A tool for interacting with the Gerrit Code Review system.",
      "mcpServers": {
        "gerrit": {
          "command": $cmd,
          "args": [ $main_py, "stdio" ],
          "env": { "PYTHON_PATH": $python_path }
        }
      },
      "contextFileName": $context_file
    }' >"${tmp_json_path}"

  mv "${tmp_json_path}" "${GEMINI_EXTENSION_JSON_PATH}"
}

# --- Main Execution ---

main() {
  local install_path=""
  local update_only_path=""
  local mode="build" # Default mode is 'build'

  # --- Argument Parsing ---
  while [[ "$#" -gt 0 ]]; do
    case $1 in
    --install-path)
      if [[ -z "${2:-}" ]]; then
        log::error "Option '--install-path' requires a value."
      fi
      install_path="$2"
      shift
      ;;
    --update-only)
      mode="update_only"
      if [[ -n "${2:-}" && ! "${2}" =~ ^- ]]; then
        update_only_path="$2"
        shift
      fi
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -?*)
      usage >&2
      log::error "Unknown option: '$1'"
      ;;
    *)
      usage >&2
      log::error "Unknown parameter passed: '$1'"
      ;;
    esac
    shift
  done

  validate_dependencies

  # --- Mode: Update Only ---
  if [[ "${mode}" == "update_only" ]]; then
    local repo_path_for_update="${update_only_path:-${HOME}/mcp-servers}"
    repo_path_for_update="$(_realpath "${repo_path_for_update}")"

    update_repo "${repo_path_for_update}"
    generate_config_file "${repo_path_for_update}/gerrit"
    log::info "Update and configuration generation complete."
    exit 0
  fi

  # --- Mode: Build (Core Logic) ---
  local is_interactive=false
  if [[ -z "${install_path}" ]]; then
    is_interactive=true
    install_path="${HOME}" # Default parent directory for the clone
  fi

  # The final repository path will be <install_path>/mcp-servers
  local install_path_abs
  install_path_abs="$(_realpath "${install_path}")"
  local repo_path="${install_path_abs}/mcp-servers"

  if [[ -d "${repo_path}" ]]; then
    if [[ ! -d "${repo_path}/.git" ]]; then
      log::error "Directory '${repo_path}' exists but is not a git repository. Please remove it or choose a different install path."
    fi
    log::info "Repository found at ${repo_path}."

    local should_update=false
    if [[ "${is_interactive}" == true ]]; then
      read -p "Do you want to update it from Git? (y/N) " -n 1 -r
      echo
      if [[ "${REPLY}" =~ ^[Yy]$ ]]; then
        should_update=true
      fi
    else
      should_update=true # Always update in non-interactive mode
    fi

    if [[ "${should_update}" == true ]]; then
      update_repo "${repo_path}"
    fi
  else
    local should_clone=false
    if [[ "${is_interactive}" == true ]]; then
      read -p "Clone 'mcp-servers' repo into ${repo_path}? (y/N) " -n 1 -r
      echo
      if [[ "${REPLY}" =~ ^[Yy]$ ]]; then
        should_clone=true
      else
        log::error "Aborting. Please provide the path using --install-path."
      fi
    else
      should_clone=true # Always clone in non-interactive mode
    fi

    if [[ "${should_clone}" == true ]]; then
      log::info "Cloning mcp-servers repository to ${repo_path}..."
      # Create the parent directory if it doesn't exist
      mkdir -p "$(dirname "${repo_path}")"
      if ! git clone sso://doc-llm-internal/mcp-servers "${repo_path}"; then
        log::error "Failed to clone repository."
      fi
      log::info "Clone successful."
    fi
  fi

  local gerrit_mcp_path="${repo_path}/gerrit"

  build_gerrit "${gerrit_mcp_path}"
  generate_config_file "${gerrit_mcp_path}"

  log::info "Successfully configured and built the Gerrit MCP server."
  log::info "Configuration written to: ${GEMINI_EXTENSION_JSON_PATH}"
}

# This ensures arguments with spaces are handled correctly.
main "$@"
