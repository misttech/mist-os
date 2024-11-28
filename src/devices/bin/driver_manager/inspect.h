// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.inspect/cpp/markers.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/binding_driver.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <atomic>

#include <fbl/array.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"

namespace driver_manager {
struct ProtocolInfo {
  const char* name;
  fbl::RefPtr<fs::PseudoDir> devnode;
  uint32_t id;
  uint32_t flags;
  uint32_t seqcount;
};

static const inline ProtocolInfo kProtoInfos[] = {
#define DDK_PROTOCOL_DEF(tag, val, name, flags) {name, nullptr, val, flags, 0},
#include <lib/ddk/protodefs.h>
};

class DeviceInspect;

class InspectManager {
 public:
  // Information that all devices end up editing.
  struct Info : fbl::RefCounted<Info> {
    Info(inspect::Node& root_node)
        : device_count(root_node.CreateUint("device_count", 0)),
          devices(root_node.CreateChild("devices")) {}

    // The total count of devices.
    inspect::UintProperty device_count;
    // The top level node for devices.
    inspect::Node devices;
  };

  explicit InspectManager(async_dispatcher_t* dispatcher);
  InspectManager() = delete;

  // Create a new device within inspect.
  DeviceInspect CreateDevice(std::string name, uint32_t protocol_id);

  // Accessor methods.
  inspect::Node& root_node() { return inspector_.root(); }

 private:
  inspect::ComponentInspector inspector_;
  fbl::RefPtr<Info> info_;
};

class DeviceInspect {
 public:
  DeviceInspect(DeviceInspect&& other) = default;
  ~DeviceInspect();

  DeviceInspect CreateChild(std::string name, uint32_t protocol_id);

  // Set the values that should not change during the life of the device.
  // This should only be called once, calling it more than once will create duplicate entries.
  void SetStaticValues(
      const std::string& topological_path, uint32_t protocol_id, const std::string& type,
      const cpp20::span<const fuchsia_driver_framework::wire::NodeProperty2>& properties,
      const std::string& driver_url);

  void set_state(const std::string& state) { state_.Set(state); }
  void set_local_id(uint64_t local_id) { local_id_.Set(local_id); }

 private:
  friend InspectManager;

  // To get a DeviceInspect object you should call InspectManager::CreateDevice.
  DeviceInspect(fbl::RefPtr<InspectManager::Info> info, std::string name, uint32_t protocol_id);

  fbl::RefPtr<InspectManager::Info> info_;

  // The inspect node for this device.
  inspect::Node device_node_;

  // Reference to nodes with static properties
  inspect::ValueList static_values_;

  inspect::StringProperty state_;
  // Unique id of the device in a driver host
  inspect::UintProperty local_id_;

  uint32_t protocol_id_;
  std::string name_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_
