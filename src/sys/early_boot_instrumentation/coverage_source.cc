// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/early_boot_instrumentation/coverage_source.h"

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.debugdata/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fdio/io.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/stdcompat/source_location.h>
#include <lib/stdcompat/span.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/service.h>
#include <lib/zx/channel.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/fidl.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <array>
#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <utility>

#include <fbl/unique_fd.h>
#include <sdk/lib/vfs/cpp/pseudo_dir.h>
#include <sdk/lib/vfs/cpp/vmo_file.h>

namespace early_boot_instrumentation {
namespace {

zx::result<> ExportAs(vfs::PseudoDir& out_dir, fbl::unique_fd fd, std::string_view export_as) {
  // Get the underlying vmo of the fd.
  zx::vmo vmo;
  if (auto res = fdio_get_vmo_exact(fd.get(), vmo.reset_and_get_address()); res != ZX_OK) {
    FX_LOGS(INFO) << 1;
    return zx::error(res);
  }
  size_t size = 0;
  if (auto res = vmo.get_prop_content_size(&size); res != ZX_OK) {
    FX_LOGS(INFO) << 2;
    return zx::error(res);
  }

  auto file = std::make_unique<vfs::VmoFile>(std::move(vmo), size);
  if (auto res = out_dir.AddEntry(std::string(export_as), std::move(file)); res != ZX_OK) {
    FX_LOGS(INFO) << 3;
    return zx::error(res);
  }

  return zx::success();
}

template <typename HandleType>
bool IsSignalled(const HandleType& handle, zx_signals_t signal) {
  zx_signals_t actual = 0;
  auto status = handle.wait_one(signal, zx::time::infinite_past(), &actual);
  return (status == ZX_OK || status == ZX_ERR_TIMED_OUT) && (actual & signal) != 0;
}

enum class DataType {
  kDynamic,
  kStatic,
};

constexpr std::string_view DataTypeDir(DataType t) {
  switch (t) {
    case DataType::kDynamic:
      return kDynamicDir;
    case DataType::kStatic:
      return kStaticDir;
    default:
      return "Unknown DataType.";
  }
}

// Returns or creates the respective instance for a given |sink_name|.
vfs::PseudoDir& GetOrCreate(std::string_view sink_name, DataType type, SinkDirMap& sink_map) {
  auto it = sink_map.find(sink_name);

  // If it's the first time we see this sink, fill up the base hierarchy:
  //  root
  //    +    /static
  //    +    /dynamic
  if (it == sink_map.end()) {
    FX_LOGS(INFO) << "Encountered sink " << sink_name << " static and dynamic subdirs created";
    it = sink_map.insert(std::make_pair(sink_name, std::make_unique<vfs::PseudoDir>())).first;
    it->second->AddEntry(std::string(kStaticDir), std::make_unique<vfs::PseudoDir>());
    it->second->AddEntry(std::string(kDynamicDir), std::make_unique<vfs::PseudoDir>());
  }

  std::string path(DataTypeDir(type));

  auto& root_dir = *(it->second);
  vfs::Node* node = nullptr;
  // Both subdirs should always be available.
  ZX_ASSERT(root_dir.Lookup(path, &node) == ZX_OK);
  return *reinterpret_cast<vfs::PseudoDir*>(node);
}

template <typename T>
void ForEachDentry(DIR* root, T&& visitor) {
  // Borrow underlying FD for opening relative files.
  int root_fd = dirfd(root);
  while (auto* dentry = readdir(root)) {
    std::string_view dentry_name(dentry->d_name);
    if (dentry_name == "." || dentry_name == "..") {
      continue;
    }

    fbl::unique_fd entry_fd(openat(root_fd, dentry->d_name, O_RDONLY));
    if (!entry_fd) {
      FX_LOGS(INFO) << "Failed to obtain FD for " << dentry->d_name << ". " << strerror(errno);
      continue;
    }

    visitor(dentry, std::move(entry_fd));
  }
}

}  // namespace

zx::result<> ExposeBootDebugdata(fbl::unique_fd& debugdata_root, SinkDirMap& sink_map) {
  // Iterate on every entry in the directory.

  DIR* root = fdopendir(debugdata_root.get());
  if (!root) {
    FX_LOGS(INFO) << "Failed to obtain DIR entry from FD. " << strerror(errno);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  // Taken by fdopendir.
  debugdata_root.release();

  auto for_each_debugdata = [&sink_map](std::string_view sink_name, DataType type,
                                        struct dirent* entry, fbl::unique_fd debugdata_fd) {
    auto res =
        ExportAs(GetOrCreate(sink_name, type, sink_map), std::move(debugdata_fd), entry->d_name);
    if (res.is_error()) {
      FX_LOGS(ERROR) << "Failed to export boot debugdata to: " << sink_name << "/"
                     << DataTypeDir(type) << "/" << entry->d_name;
      return;
    }

    FX_LOGS(INFO) << " Exported boot debugdata to " << sink_name << "/" << DataTypeDir(type) << "/"
                  << entry->d_name;
  };

  // Each sink contains at most two entries, mapped to either static or dynamic data.
  // "s" or "d", each of them a directory.
  auto for_each_sink = [&for_each_debugdata](struct dirent* entry, fbl::unique_fd sink_dir_fd) {
    std::string_view sink_name(entry->d_name);

    fbl::unique_fd static_dir_fd(openat(sink_dir_fd.get(), "s", O_DIRECTORY | O_RDONLY));
    if (static_dir_fd) {
      DIR* static_dir = fdopendir(static_dir_fd.get());
      if (static_dir) {
        static_dir_fd.release();
        ForEachDentry(static_dir, [&](auto* dentry, fbl::unique_fd debugdata_fd) {
          for_each_debugdata(sink_name, DataType::kStatic, dentry, std::move(debugdata_fd));
        });
        closedir(static_dir);
      }
    }

    fbl::unique_fd dynamic_dir_fd(openat(sink_dir_fd.get(), "d", O_DIRECTORY | O_RDONLY));
    if (dynamic_dir_fd) {
      DIR* dynamic_dir = fdopendir(dynamic_dir_fd.release());
      if (dynamic_dir) {
        dynamic_dir_fd.release();
        ForEachDentry(dynamic_dir, [&](auto* dentry, fbl::unique_fd debugdata_fd) {
          for_each_debugdata(sink_name, DataType::kDynamic, dentry, std::move(debugdata_fd));
        });
        closedir(dynamic_dir);
      }
    }
  };

  // Each entry in the root is a directory named after the sink.
  ForEachDentry(root, for_each_sink);
  closedir(root);
  return zx::success();
}

namespace {

constexpr const char* kPublisherPath = fidl::DiscoverableProtocolName<fuchsia_debugdata::Publisher>;

/// Server which forwards requests for the debugdata.Publisher service to the Connect function.
class StashPublisherBridge : public fidl::testing::WireTestBase<fuchsia_io::Directory> {
 public:
  virtual void Connect(fidl::ServerEnd<fuchsia_debugdata::Publisher> server_end) = 0;

 private:
  void HandleOpenRequest(std::string_view path, zx::channel channel) {
    if (path == kPublisherPath) {
      Connect(fidl::ServerEnd<fuchsia_debugdata::Publisher>{std::move(channel)});
    } else {
      FX_LOGS(WARNING) << "Encountered open request to unhandled path: " << path;
    }
  }

  void DeprecatedOpen(DeprecatedOpenRequestView request,
                      DeprecatedOpenCompleter::Sync& completer) final {
    HandleOpenRequest(request->path.get(), request->object.TakeChannel());
  }

  void Open(OpenRequestView request, OpenCompleter::Sync& completer) final {
    HandleOpenRequest(request->path.get(), std::move(request->object));
  }

  // We use wire test base to avoid churn when new directory methods are added/removed. These
  // methods should never be called, since we only support opening a specific path.
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    FX_LOGS(ERROR) << "Unsupported method: " << name;
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

class Server : public StashPublisherBridge,
               public fidl::WireServer<fuchsia_boot::SvcStash>,
               public fidl::WireServer<fuchsia_debugdata::Publisher> {
 public:
  explicit Server(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  SinkDirMap TakeSinkToDir() { return std::move(sink_to_dir_); }

 private:
  void Publish(PublishRequestView request, PublishCompleter::Sync& completer) override {
    DataType published_data_type = IsSignalled(request->vmo_token, ZX_EVENTPAIR_PEER_CLOSED)
                                       ? DataType::kStatic
                                       : DataType::kDynamic;
    auto& dir = GetOrCreate(request->data_sink.get(), published_data_type, sink_to_dir_);
    std::array<char, ZX_MAX_NAME_LEN> name_buff = {};
    auto name = std::to_string(svc_id_) + "-" + std::to_string(req_id_);
    if (zx_status_t status =
            request->data.get_property(ZX_PROP_NAME, name_buff.data(), name_buff.size());
        status == ZX_OK) {
      std::string name_prop(name_buff.data());
      if (!name_prop.empty()) {
        name += "." + name_prop;
      }
    }
    uint64_t size;
    if (zx_status_t status = request->data.get_prop_content_size(&size); status != ZX_OK) {
      FX_PLOGS(INFO, status) << "Failed to obtain vmo content size. Attempting to use vmo size.";
      if (zx_status_t status = request->data.get_size(&size); status != ZX_OK) {
        FX_PLOGS(INFO, status) << "Failed to obtain vmo size.";
        size = 0;
      }
    }
    FX_LOGS(INFO) << "Exposing " << request->data_sink.get() << "/"
                  << (published_data_type == DataType::kStatic ? "static/" : "dynamic/") << name
                  << " size: " << size << " bytes";
    dir.AddEntry(std::move(name), std::make_unique<vfs::VmoFile>(std::move(request->data), size));
    ++req_id_;
  }

  void Connect(fidl::ServerEnd<fuchsia_debugdata::Publisher> server_end) override {
    FX_LOGS(INFO) << "Encountered open request to " << kPublisherPath;
    publisher_bindings_.AddBinding(dispatcher_, std::move(server_end), this,
                                   fidl::kIgnoreBindingClosure);
  }

  void Store(StoreRequestView request, StoreCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "Encountered stashed svc handle";
    directory_bindings_.AddBinding(dispatcher_, std::move(request->svc_endpoint), this,
                                   [](Server* impl, fidl::UnbindInfo) {
                                     impl->req_id_ = 0;
                                     impl->svc_id_++;
                                   });
  }

  async_dispatcher_t* const dispatcher_;
  SinkDirMap sink_to_dir_;

  // used for name generation.
  int svc_id_ = 0;
  int req_id_ = 0;

  fidl::ServerBindingGroup<fuchsia_io::Directory> directory_bindings_;
  fidl::ServerBindingGroup<fuchsia_debugdata::Publisher> publisher_bindings_;
};

}  // namespace

SinkDirMap ExtractDebugData(fidl::ServerEnd<fuchsia_boot::SvcStash> svc_stash) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  Server server(loop.dispatcher());
  fidl::BindServer(loop.dispatcher(), std::move(svc_stash), &server);
  loop.RunUntilIdle();
  return server.TakeSinkToDir();
}

zx::result<> ExposeLogs(fbl::unique_fd& boot_logs_dir, vfs::PseudoDir& out_dir) {
  if (!boot_logs_dir) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  DIR* root = fdopendir(boot_logs_dir.get());
  if (!root) {
    FX_LOGS(INFO) << "Failed to obtain DIR entry from FD. " << strerror(errno);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  // Taken by fdopendir.
  boot_logs_dir.release();

  ForEachDentry(root, [&out_dir](struct dirent* entry, fbl::unique_fd fd) {
    std::string_view entry_name(entry->d_name);
    if (entry->d_type != DT_REG) {
      return;
    }
    if (ExportAs(out_dir, std::move(fd), entry_name).is_ok()) {
      FX_LOGS(INFO) << "Log file " << entry_name << " exposed on log dir.";
    } else {
      FX_LOGS(INFO) << "Log file " << entry_name << " found but failed to be exposed.";
    }
  });
  closedir(root);

  return zx::ok();
}

}  // namespace early_boot_instrumentation
