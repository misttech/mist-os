// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_TYPES_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_TYPES_H_

namespace driver_manager {

enum class Collection : uint8_t {
  kNone,
  // Collection for boot drivers.
  kBoot,
  // Collection for package drivers.
  kPackage,
  // Collection for universe package drivers.
  kFullPackage,
};

enum class NodeType {
  kNormal,     // Normal non-composite node.
  kComposite,  // Composite node created from composite node specs.
};

enum class DriverState : uint8_t {
  kStopped,  // Driver is not running.
  kBinding,  // Driver's bind hook is scheduled and running.
  kRunning,  // Driver finished binding and is running.
};

enum class NodeState : uint8_t {
  kRunning,              // Normal running state.
  kPrestop,              // Still running, but will remove soon. usually because the node
                         // received Remove(kPackage), but is a boot driver.
  kWaitingOnDriverBind,  // Waiting for the driver to complete binding.
  kWaitingOnChildren,    // Received Remove, and waiting for children to be removed.

  kWaitingOnDriver,           // Waiting for driver to respond from Stop() command.
  kWaitingOnDriverComponent,  // Waiting driver component to be destroyed.
  kStopped,                   // Node finished shutdown.
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_TYPES_H_
