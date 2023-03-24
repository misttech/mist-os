// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/early_boot_instrumentation/coverage_source.h"

#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.debugdata/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
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
#include <mutex>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>

#include <fbl/unique_fd.h>
#include <sdk/lib/vfs/cpp/pseudo_dir.h>
#include <sdk/lib/vfs/cpp/vmo_file.h>

namespace early_boot_instrumentation {
namespace {

constexpr std::string_view kKernelProfRaw = "zircon.elf.profraw";
constexpr std::string_view kKernelSymbolizerLog = "symbolizer.log";

constexpr std::string_view kPhysbootProfRaw = "physboot.profraw";
constexpr std::string_view kPhysbootSymbolizerLog = "symbolizer.log";

struct ExportedFd {
  fbl::unique_fd fd;
  std::string export_name;
};

zx::result<> Export(vfs::PseudoDir& out_dir, cpp20::span<ExportedFd> exported_fds) {
  for (const auto& [fd, export_as] : exported_fds) {
    // Get the underlying vmo of the fd.
    zx::vmo vmo;
    if (auto res = fdio_get_vmo_exact(fd.get(), vmo.reset_and_get_address()); res != ZX_OK) {
      return zx::error(res);
    }
    size_t size = 0;
    if (auto res = vmo.get_size(&size); res != ZX_OK) {
      return zx::error(res);
    }

    auto file = std::make_unique<vfs::VmoFile>(std::move(vmo), size);
    if (auto res = out_dir.AddEntry(export_as, std::move(file)); res != ZX_OK) {
      return zx::error(res);
    }
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

  std::string path(type == DataType::kDynamic ? kDynamicDir : kStaticDir);

  auto& root_dir = *(it->second);
  vfs::internal::Node* node = nullptr;
  // Both subdirs should always be available.
  ZX_ASSERT(root_dir.Lookup(path, &node) == ZX_OK);
  ZX_ASSERT(node->IsDirectory());
  return *reinterpret_cast<vfs::PseudoDir*>(node);
}

}  // namespace

zx::result<> ExposeKernelProfileData(fbl::unique_fd& kernel_data_dir, SinkDirMap& sink_map) {
  std::vector<ExportedFd> exported_fds;

  fbl::unique_fd kernel_profile(openat(kernel_data_dir.get(), kKernelProfRaw.data(), O_RDONLY));
  if (!kernel_profile) {
    if (errno == ENOENT) {
      // This file is only available in instrumented builds.
      FX_LOGS(INFO) << kKernelProfRaw << " is not available.";
      // return success as there is nothing to export.
      return zx::success();
    }
    const char* err = strerror(errno);
    FX_LOGS(ERROR) << "Could not obtain handle to " << kKernelProfRaw << ": " << err;
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  exported_fds.emplace_back(
      ExportedFd{.fd = std::move(kernel_profile), .export_name = std::string(kKernelFile)});
  FX_LOGS(INFO) << "Exposing " << kKernelFile;

  fbl::unique_fd kernel_log(openat(kernel_data_dir.get(), kKernelSymbolizerLog.data(), O_RDONLY));
  if (kernel_log) {
    FX_LOGS(INFO) << "Exposing " << kKernelSymbolizerFile;
    exported_fds.emplace_back(
        ExportedFd{.fd = std::move(kernel_log), .export_name = std::string(kKernelSymbolizerFile)});
  }

  return Export(GetOrCreate(kLlvmSink, DataType::kDynamic, sink_map), exported_fds);
}

zx::result<> ExposePhysbootProfileData(fbl::unique_fd& physboot_data_dir, SinkDirMap& sink_map) {
  std::vector<ExportedFd> exported_fds;

  fbl::unique_fd phys_profile(openat(physboot_data_dir.get(), kPhysbootProfRaw.data(), O_RDONLY));
  if (!phys_profile) {
    if (errno == ENOENT) {
      // This file is only available in instrumented builds.
      FX_LOGS(INFO) << kPhysbootProfRaw << " is not available.";
      // return success as there is nothing to export.
      return zx::success();
    }
    const char* err = strerror(errno);
    FX_LOGS(ERROR) << "Could not obtain handle to " << kPhysbootProfRaw << ": " << err;
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  exported_fds.emplace_back(
      ExportedFd{.fd = std::move(phys_profile), .export_name = std::string(kPhysFile)});
  FX_LOGS(INFO) << "Exposing " << kPhysFile;

  fbl::unique_fd phys_log(openat(physboot_data_dir.get(), kPhysbootSymbolizerLog.data(), O_RDONLY));
  if (phys_log) {
    FX_LOGS(INFO) << "Exposing " << kPhysSymbolizerFile;
    exported_fds.emplace_back(
        ExportedFd{.fd = std::move(phys_log), .export_name = std::string(kPhysSymbolizerFile)});
  }

  return Export(GetOrCreate(kLlvmSink, DataType::kStatic, sink_map), exported_fds);
}

namespace {

class Server : public fidl::WireServer<fuchsia_boot::SvcStash>,
               public fidl::WireServer<fuchsia_io::Openable>,
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

  void Open(OpenRequestView request, OpenCompleter::Sync& completer) override {
    if (request->path.get() == fidl::DiscoverableProtocolName<fuchsia_debugdata::Publisher>) {
      FX_LOGS(INFO) << "Encountered open request to debugdata.Publisher";
      publisher_bindings_.AddBinding(
          dispatcher_, fidl::ServerEnd<fuchsia_debugdata::Publisher>{request->object.TakeChannel()},
          this, fidl::kIgnoreBindingClosure);
    } else {
      FX_LOGS(INFO) << "Encountered open request to unhandled path: " << request->path.get();
    }
  }

  void Store(StoreRequestView request, StoreCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "Encountered stashed svc handle";
    fidl::ServerEnd<fuchsia_io::Directory>& directory = request->svc_endpoint;
    openable_bindings_.AddBinding(dispatcher_,
                                  fidl::ServerEnd<fuchsia_io::Openable>{directory.TakeChannel()},
                                  this, [](Server* impl, fidl::UnbindInfo) {
                                    impl->req_id_ = 0;
                                    impl->svc_id_++;
                                  });
  }

  async_dispatcher_t* const dispatcher_;
  SinkDirMap sink_to_dir_;

  // used for name generation.
  int svc_id_ = 0;
  int req_id_ = 0;

  fidl::ServerBindingGroup<fuchsia_io::Openable> openable_bindings_;
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

}  // namespace early_boot_instrumentation
