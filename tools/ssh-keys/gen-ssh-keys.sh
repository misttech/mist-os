#!/usr/bin/env bash
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

readonly SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly FUCHSIA_ROOT="${SCRIPT_ROOT}/../.."

function move_ssh_keys {
  local orig_key dest_key orig_auth_keys dest_auth_keys dest_dir
  orig_key="$1"
  dest_key="$2"
  orig_auth_keys="$3"
  dest_auth_keys="$4"

  dest_dir="$(dirname "${dest_key}")"

  (
    # force subshell to limit scope of umask
    umask 077 && mkdir -p "${dest_dir}"
    umask 177
    mv "${orig_key}" "${dest_key}"
    umask 133

    # Documentation previously recommended copying default SSH identity files
    # (private keys) and authorized_keys files, but not public key files. When
    # missing, these are skipped.
    if [[ -f "${orig_key}.pub" ]]; then
      mv "${orig_key}.pub" "${dest_key}.pub"
    fi
    cat "${orig_auth_keys}" >>"${dest_auth_keys}"
    rm "${orig_auth_keys}"
  )
}

function normalize_local_keys {
  local intree="${FUCHSIA_ROOT}/.ssh/pkey"
  local inhome="${HOME}/.ssh/fuchsia_ed25519"

  local have_in_tree=false
  local have_in_home=false
  test -f "$intree" && have_in_tree=true
  test -f "$inhome" && have_in_home=true

  if $have_in_tree && ! $have_in_home; then
    echo "Copying existing SSH credentials from //.ssh/pkey to ~/.ssh/fuchsia_ed25519"
    move_ssh_keys "$intree" "$inhome" \
      "${FUCHSIA_ROOT}/.ssh/authorized_keys" \
      "${HOME}/.ssh/fuchsia_authorized_keys"
  elif $have_in_tree && $have_in_home; then
    rm "$intree"
  fi
}

# $INFRA_RECIPES is a variable set by infra bots.
if [[ "${INFRA_RECIPES}" == "1" ]]; then
  fx-error "gen-ssh-keys.sh should not be run by infra bots"
  exit 0
fi


# This is a tranisitional script that copies the in-tree ssh key to
# ~/.ssh if one does not already exist. If neither exist, no action is
# taken at this point
normalize_local_keys
