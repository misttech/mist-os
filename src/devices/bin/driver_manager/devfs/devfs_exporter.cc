// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/devfs/devfs_exporter.h"

namespace fdfs = fuchsia_device_fs;

namespace driver_manager {

zx::result<std::unique_ptr<ExportWatcher>> ExportWatcher::Create(
    async_dispatcher_t* dispatcher, Devfs& devfs, Devnode* root,
    fidl::ClientEnd<fuchsia_device_fs::Connector> connector,
    std::optional<std::string> topological_path, std::optional<std::string> class_name) {
  std::unique_ptr<ExportWatcher> watcher{new ExportWatcher(topological_path)};

  zx_status_t status = root->export_dir(
      Devnode::Target(Devnode::Connector{
          .connector = fidl::WireSharedClient(std::move(connector), dispatcher, watcher.get()),
      }),
      std::move(topological_path), std::move(class_name), watcher->devnodes_);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(watcher));
}

DevfsExporter::DevfsExporter(Devfs& devfs, Devnode* root, async_dispatcher_t* dispatcher)
    : devfs_(devfs), root_(root), dispatcher_(dispatcher) {}

void DevfsExporter::PublishExporter(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<fdfs::Exporter>(
      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());
}

void DevfsExporter::Export(ExportRequestView request, ExportCompleter::Sync& completer) {
  std::optional<std::string> topological_path;
  if (!request->topological_path.is_null()) {
    topological_path.emplace(request->topological_path.get());
  }

  std::optional<std::string> class_name;
  if (!request->class_name.is_null()) {
    class_name.emplace(request->class_name.get());
  }

  zx::result result =
      ExportWatcher::Create(dispatcher_, devfs_, root_, std::move(request->open_client),
                            std::move(topological_path), std::move(class_name));
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }
  AddWatcher(std::move(result.value()));
  completer.ReplySuccess();
}

void DevfsExporter::AddWatcher(std::unique_ptr<ExportWatcher> watcher) {
  // Set a callback so when the ExportWatcher sees its connection closed, it
  // will delete itself and its devnodes.
  watcher->set_on_close_callback([this](ExportWatcher* self) { exports_.erase(self); });
  auto [it, inserted] = exports_.try_emplace(watcher.get(), std::move(watcher));
  ZX_ASSERT_MSG(inserted, "duplicate pointer %p", it->first);
}

}  // namespace driver_manager
