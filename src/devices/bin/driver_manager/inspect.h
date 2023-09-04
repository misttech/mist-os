// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_

#include <lib/ddk/binding.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <fbl/array.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vmo_file.h"

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

class InspectDevfs {
 public:
  static zx::result<InspectDevfs> Create(fbl::RefPtr<fs::PseudoDir> root_dir);

  // Publishes a device. Should be called when there's a new devices.
  // This returns a string, `link_name`, representing the device that was just published.
  // This string should be passed to `Unpublish` to remove the device when it's being removed.
  zx::result<std::string> Publish(uint32_t protocol_id, const char* name,
                                  fbl::RefPtr<fs::VmoFile> file);

  // Unpublishes a device. Should be called when a device is being removed.
  void Unpublish(uint32_t protocol_id, fbl::RefPtr<fs::VmoFile> file, std::string_view link_name);

  std::tuple<fbl::RefPtr<fs::PseudoDir>, uint32_t*> GetProtoDir(uint32_t id);

 private:
  explicit InspectDevfs(fbl::RefPtr<fs::PseudoDir> root_dir, fbl::RefPtr<fs::PseudoDir> class_dir);

  // Delete protocol |id| directory if no files are present.
  void RemoveEmptyProtoDir(uint32_t id);

  // Get protocol |id| directory if it exists, else create one.
  std::tuple<fbl::RefPtr<fs::PseudoDir>, uint32_t*> GetOrCreateProtoDir(uint32_t id);

  fbl::RefPtr<fs::PseudoDir> root_dir_;
  fbl::RefPtr<fs::PseudoDir> class_dir_;
  std::array<ProtocolInfo, std::size(kProtoInfos)> proto_infos_;
};

class DeviceInspect;

class InspectManager {
 public:
  // Information that all devices end up editing.
  struct Info : fbl::RefCounted<Info> {
    Info(inspect::Node& root_node, InspectDevfs inspect_devfs)
        : device_count(root_node.CreateUint("device_count", 0)),
          devices(root_node.CreateChild("devices")),
          devfs(std::move(inspect_devfs)) {}

    // The total count of devices.
    inspect::UintProperty device_count;
    // The top level node for devices.
    inspect::Node devices;
    // The inspect nodes for devfs information.
    InspectDevfs devfs;
  };

  explicit InspectManager(async_dispatcher_t* dispatcher);
  InspectManager() = delete;

  // Create a new device within inspect.
  DeviceInspect CreateDevice(std::string name, zx::vmo vmo, uint32_t protocol_id);

  // Start serving the inspect directory.
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Connect();

  // Accessor methods.
  fs::PseudoDir& diagnostics_dir() { return *diagnostics_dir_; }
  fbl::RefPtr<fs::PseudoDir> driver_host_dir() { return driver_host_dir_; }
  inspect::Node& root_node() { return inspector_.GetRoot(); }
  inspect::Inspector& inspector() { return inspector_; }
  InspectDevfs& devfs() { return info_->devfs; }

 private:
  inspect::Inspector inspector_;

  std::unique_ptr<fs::SynchronousVfs> diagnostics_vfs_;
  fbl::RefPtr<fs::PseudoDir> diagnostics_dir_ = fbl::MakeRefCounted<fs::PseudoDir>();
  fbl::RefPtr<fs::PseudoDir> driver_host_dir_ = fbl::MakeRefCounted<fs::PseudoDir>();

  fbl::RefPtr<Info> info_;
};

class DeviceInspect {
 public:
  DeviceInspect(DeviceInspect&& other) = default;
  ~DeviceInspect();

  DeviceInspect CreateChild(std::string name, zx::vmo vmo, uint32_t protocol_id);

  // Publish this Device. The device will be automatically unpublished when it is destructed.
  zx::result<> Publish();

  // Set the values that should not change during the life of the device.
  // This should only be called once, calling it more than once will create duplicate entries.
  void SetStaticValues(const std::string& topological_path, uint32_t protocol_id,
                       const std::string& type, uint32_t flags,
                       const cpp20::span<const zx_device_prop_t>& properties,
                       const std::string& driver_url);

  void set_state(const std::string& state) { state_.Set(state); }
  void set_local_id(uint64_t local_id) { local_id_.Set(local_id); }

 private:
  friend InspectManager;

  // To get a DeviceInspect object you should call InspectManager::CreateDevice.
  DeviceInspect(fbl::RefPtr<InspectManager::Info> info, std::string name, zx::vmo vmo,
                uint32_t protocol_id);

  fbl::RefPtr<InspectManager::Info> info_;

  // The inspect node for this device.
  inspect::Node device_node_;

  // Reference to nodes with static properties
  inspect::ValueList static_values_;

  inspect::StringProperty state_;
  // Unique id of the device in a driver host
  inspect::UintProperty local_id_;

  // Inspect VMO returned via devfs's inspect nodes.
  std::optional<fbl::RefPtr<fs::VmoFile>> vmo_file_;

  uint32_t protocol_id_;
  std::string name_;
  std::string link_name_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_INSPECT_H_
