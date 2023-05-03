// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/devfs/allowlist.h"

#include <unordered_set>

bool AllowMultiplexingController(std::string_view class_name) {
  // TODO(https://fxbug.dev/112484): Remove entries from this list.
  static const std::unordered_set<std::string_view> classes_that_include_controller({
      "block",
      "acpi",
      "bt-hci",
      "bt-emulator",
      "bt-host",
      "media-codec",
      "thermal",
      "nand",
      "skip-block",
      "rtc",
      "sysmem",
      "tdh",
      "cpu-ctrl",
      "network",
      "virtual-bus-test",
      "temperature",
  });
  return classes_that_include_controller.find(class_name) != classes_that_include_controller.end();
}

bool AllowMultiplexingNode(std::string_view class_name) {
  // TODO(https://fxbug.dev/112484): Remove entries from this list.
  static const std::unordered_set<std::string_view> classes_that_include_node({
      "block",
      "goldfish-pipe",
      "skip-block",
      "sysmem",
      "ot-radio",
  });
  return classes_that_include_node.find(class_name) != classes_that_include_node.end();
}
