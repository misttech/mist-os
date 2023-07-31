// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_ALLOWLIST_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_ALLOWLIST_H_

#include <string>

// This takes in a class name and returns true iff the entries in `/dev/class/{class_name}` should
// have fuchsia.io/Node multiplexed on the channel.
bool AllowMultiplexingNode(std::string_view class_name);

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_ALLOWLIST_H_
