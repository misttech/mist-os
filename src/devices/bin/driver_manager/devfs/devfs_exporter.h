// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_EXPORTER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_EXPORTER_H_

#include <fidl/fuchsia.device.fs/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include "src/devices/bin/driver_manager/devfs/devfs.h"

namespace driver_manager {

// Each ExportWatcher represents one call to `DevfsExporter::Export`. It holds all of the
// nodes created by that export.
class ExportWatcher : public fidl::WireAsyncEventHandler<fuchsia_io::Node>,
                      public fidl::WireAsyncEventHandler<fuchsia_device_fs::Connector> {
 public:
  // Create an ExportWatcher with a connector.
  static zx::result<std::unique_ptr<ExportWatcher>> Create(
      async_dispatcher_t* dispatcher, Devfs& devfs, Devnode* root,
      fidl::ClientEnd<fuchsia_device_fs::Connector> connector,
      std::optional<std::string> topological_path, std::optional<std::string> class_name,
      fuchsia_device_fs::wire::ExportOptions options);

  void set_on_close_callback(fit::callback<void(ExportWatcher*)> callback) {
    callback_ = std::move(callback);
  }

  void on_fidl_error(fidl::UnbindInfo error) override {
    if (callback_) {
      callback_(this);
    }
  }

  const std::optional<std::string>& topological_path() const { return topological_path_; }

  zx::result<> MakeVisible();

 private:
  explicit ExportWatcher(std::optional<std::string> topological_path)
      : topological_path_(std::move(topological_path)) {}

  fit::callback<void(ExportWatcher*)> callback_;
  fidl::WireClient<fuchsia_io::Node> client_;
  std::vector<std::unique_ptr<Devnode>> devnodes_;
  const std::optional<std::string> topological_path_;
};

class DevfsExporter : public fidl::WireServer<fuchsia_device_fs::Exporter> {
 public:
  // The `root` Devnode must outlive `this`.
  DevfsExporter(Devfs& devfs, Devnode* root, async_dispatcher_t* dispatcher);

  void PublishExporter(component::OutgoingDirectory& outgoing);

 private:
  // fidl::WireServer<fuchsia_device_fs::Exporter>
  void ExportV2(ExportV2RequestView request, ExportV2Completer::Sync& completer) override;

  void MakeVisible(MakeVisibleRequestView request, MakeVisibleCompleter::Sync& completer) override;

  void AddWatcher(std::unique_ptr<ExportWatcher> watcher);

  Devfs& devfs_;
  Devnode* const root_;
  fidl::ServerBindingGroup<fuchsia_device_fs::Exporter> bindings_;
  async_dispatcher_t* const dispatcher_;

  std::unordered_map<ExportWatcher*, std::unique_ptr<ExportWatcher>> exports_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_EXPORTER_H_
