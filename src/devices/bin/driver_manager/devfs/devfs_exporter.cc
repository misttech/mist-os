// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/devfs/devfs_exporter.h"

#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/split_string.h"

namespace fdfs = fuchsia_device_fs;

namespace driver_manager {

zx::result<std::unique_ptr<ExportWatcher>> ExportWatcher::Create(
    async_dispatcher_t* dispatcher, Devfs& devfs, Devnode* root,
    fidl::ClientEnd<fuchsia_io::Directory> service_dir, std::string_view service_path,
    std::string_view devfs_path, uint32_t protocol_id,
    fuchsia_device_fs::wire::ExportOptions options) {
  // Validate service path.
  {
    const std::vector segments =
        fxl::SplitString(service_path, "/", fxl::WhiteSpaceHandling::kKeepWhitespace,
                         fxl::SplitResult::kSplitWantAll);
    if (segments.empty() ||
        std::any_of(segments.begin(), segments.end(), std::mem_fn(&std::string_view::empty))) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }

  auto response =
      fidl::WireCall(service_dir)
          ->Open(fuchsia_io::wire::OpenFlags::kRightReadable |
                     fuchsia_io::wire::OpenFlags::kRightWritable,
                 0, fidl::StringView::FromExternal(service_path), std::move(endpoints->server));
  if (!response.ok()) {
    return zx::error(response.error().status());
  }

  std::unique_ptr<ExportWatcher> watcher{new ExportWatcher(std::string(devfs_path))};
  watcher->client_ = fidl::WireClient(std::move(endpoints->client), dispatcher, watcher.get());

  zx_status_t status =
      root->export_dir(Devnode::Target(Devnode::Service{
                           .remote = std::move(service_dir),
                           .path = std::string(service_path),
                           .export_options = options,
                       }),
                       devfs_path, ProtocolIdToClassName(protocol_id), watcher->devnodes_);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(watcher));
}

zx::result<> ExportWatcher::MakeVisible() {
  for (auto& node : devnodes_) {
    Devnode::ExportOptions* options = node->export_options();
    if (*options != fuchsia_device_fs::wire::ExportOptions::kInvisible) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    *options -= fuchsia_device_fs::wire::ExportOptions::kInvisible;
    node->publish();
  }

  return zx::ok();
}

zx::result<std::unique_ptr<ExportWatcher>> ExportWatcher::Create(
    async_dispatcher_t* dispatcher, Devfs& devfs, Devnode* root,
    fidl::ClientEnd<fuchsia_device_fs::Connector> connector,
    std::optional<std::string> topological_path, std::optional<std::string> class_name,
    fuchsia_device_fs::wire::ExportOptions options) {
  std::unique_ptr<ExportWatcher> watcher{new ExportWatcher(topological_path)};

  zx_status_t status = root->export_dir(
      Devnode::Target(Devnode::Connector{
          .connector = fidl::WireSharedClient(std::move(connector), dispatcher, watcher.get()),
          .export_options = options,
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

zx::result<> DevfsExporter::Export(fidl::ClientEnd<fuchsia_io::Directory> service_dir,
                                   std::string_view service_path, std::string_view devfs_path,
                                   uint32_t protocol_id,
                                   fuchsia_device_fs::wire::ExportOptions options) {
  zx::result result = ExportWatcher::Create(dispatcher_, devfs_, root_, std::move(service_dir),
                                            service_path, devfs_path, protocol_id, options);
  if (result.is_error()) {
    LOGF(ERROR, "Failed to export service to devfs path \"%s\": %s",
         std::string(devfs_path).c_str(), result.status_string());
    return result.take_error();
  }
  AddWatcher(std::move(result.value()));
  return zx::ok();
}

void DevfsExporter::Export(ExportRequestView request, ExportCompleter::Sync& completer) {
  completer.Reply(Export(std::move(request->service_dir), request->service_path.get(),
                         request->devfs_path.get(), request->protocol_id,
                         fuchsia_device_fs::wire::ExportOptions()));
}

void DevfsExporter::ExportOptions(ExportOptionsRequestView request,
                                  ExportOptionsCompleter::Sync& completer) {
  completer.Reply(Export(std::move(request->service_dir), request->service_path.get(),
                         request->devfs_path.get(), request->protocol_id, request->options));
}

void DevfsExporter::ExportV2(ExportV2RequestView request, ExportV2Completer::Sync& completer) {
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
                            std::move(topological_path), std::move(class_name), request->options);
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }
  AddWatcher(std::move(result.value()));
  completer.ReplySuccess();
}

void DevfsExporter::MakeVisible(MakeVisibleRequestView request,
                                MakeVisibleCompleter::Sync& completer) {
  for (auto& [k, e] : exports_) {
    if (e->topological_path() != request->devfs_path.get()) {
      continue;
    }
    completer.Reply(e->MakeVisible());
    return;
  }
  completer.ReplyError(ZX_ERR_NOT_FOUND);
}

void DevfsExporter::AddWatcher(std::unique_ptr<ExportWatcher> watcher) {
  // Set a callback so when the ExportWatcher sees its connection closed, it
  // will delete itself and its devnodes.
  watcher->set_on_close_callback([this](ExportWatcher* self) { exports_.erase(self); });
  auto [it, inserted] = exports_.try_emplace(watcher.get(), std::move(watcher));
  ZX_ASSERT_MSG(inserted, "duplicate pointer %p", it->first);
}

}  // namespace driver_manager
