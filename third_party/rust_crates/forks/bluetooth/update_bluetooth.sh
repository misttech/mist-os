#!/bin/bash
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

export TMP_CHECKOUT=$(mktemp -d)

if [[ $# -ne 1 || $1 == "--help" ]]; then
  echo "Usage: ./update_bluetooth.sh [git_ref]";
  exit -1
fi

git_ref=$1;

git clone -n --depth=1 --filter=tree:0 https://bluetooth.googlesource.com/bluetooth $TMP_CHECKOUT

bluetooth_mirror_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd);

echo "Checking out $git_ref into $bluetooth_mirror_dir...";

cd $TMP_CHECKOUT;

git fetch https://bluetooth.googlesource.com/bluetooth $git_ref

git sparse-checkout set --no-cone /rust LICENSE
git checkout --quiet FETCH_HEAD -- .

licenses_to_solidify=$(find rust -name LICENSE);

for target in $licenses_to_solidify
do
  cp --remove-destination LICENSE $target
done

files_to_persist=(README.fuchsia update_bluetooth.sh)

for persist in ${files_to_persist[@]}
do
  cp -v $bluetooth_mirror_dir/$persist $TMP_CHECKOUT/rust/
done

#
# Update the Cargo.toml in the crates to remove all of *.workspace = true
# since rules_rust can't find the workspace file.
# See https://github.com/bazelbuild/rules_rust/issues/3059

crate_paths=rust/bt-*

# Crate versions come from Cargo.toml
declare -A external_crate_versions=(
  [assert_matches]="\"1.5.0\""
  [bitfield]="\"0.14.0\""
  [futures]="\"=0.3.30\""
  [lazy_static]="\"1.4\""
  [log]="{ version = \"0.4.22\", features = [ \"kv\", \"std\" ] }"
  [parking_lot]="\"0.12.0\""
  [pretty_assertions]="\"1.2.1\""
  [thiserror]="\"2.0.11\""
  [uuid]="{ version = \"1.1.2\", features = [\"serde\", \"v4\"] }"
)

for crate in ${crate_paths[@]}
do
  sed -i -e 's/edition.workspace = true/edition = "2021"/' $crate/Cargo.toml
  sed -i -e 's/license.workspace = true/license = "BSD-2-Clause"/' $crate/Cargo.toml
  sed -i -e 's/license.workspace = true/license = "BSD-2-Clause"/' $crate/Cargo.toml
  for internal_crate_name in $crate_paths
  do
    crate_name=${internal_crate_name#"rust/"}
    sed -i -e "s/$crate_name.workspace = true/$crate_name = { path = \"..\/$crate_name\" }/" $crate/Cargo.toml
  done
  for external_crate_name in "${!external_crate_versions[@]}"
  do
    sed -i -e "s/$external_crate_name.workspace = true/$external_crate_name = ${external_crate_versions[$external_crate_name]}/" $crate/Cargo.toml
  done
  # bt-gatt also has a dev-dependency we need to update
  sed -i -e "s/bt-gatt = { workspace = true, /bt-gatt = { path = \"..\/bt-gatt\", /" $crate/Cargo.toml
done

rm -r $bluetooth_mirror_dir
mkdir $bluetooth_mirror_dir
cp -rv rust/* $bluetooth_mirror_dir/


echo "Done"
