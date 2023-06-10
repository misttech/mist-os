// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This component hosts a memfs instance that implements `fuchsia.fs.Admin` and
// `fuchsia.fs.startup.Startup`. Memfs is run as a separate component in benchmarks to be able to
// better compare its results with other filesystems that will also be running as separate
// components. Only one memfs instance can be running at a time.

#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstdlib>
#include <memory>
#include <mutex>

#include <fbl/ref_ptr.h>

#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"

using component::OutgoingDirectory;
namespace fio = fuchsia_io;
constexpr std::string_view kFsRoot = "root";

class MemfsHandler {
 public:
  explicit MemfsHandler(OutgoingDirectory& outgoing_directory)
      : loop_(&kAsyncLoopConfigNeverAttachToThread), outgoing_directory_(outgoing_directory) {
    ZX_ASSERT(loop_.StartThread("memfs-serving-thread") == ZX_OK);
  }

  zx::result<> Start() {
    std::scoped_lock guard{mutex_};
    if (memfs_ != nullptr) {
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }

    auto endpoints = fidl::CreateEndpoints<fio::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    zx::result result = memfs::Memfs::Create(loop_.dispatcher(), "memfs");
    if (result.is_error()) {
      return result.take_error();
    }
    auto& [memfs, root] = result.value();
    memfs_ = std::move(memfs);
    memfs_->ServeDirectory(root, std::move(endpoints->server));

    return outgoing_directory_.AddDirectory(std::move(endpoints->client), kFsRoot);
  }

  void Stop() {
    std::scoped_lock guard{mutex_};
    ZX_ASSERT(memfs_ != nullptr);

    ZX_ASSERT(outgoing_directory_.RemoveDirectory(kFsRoot).is_ok());

    libsync::Completion completer;
    memfs_->Shutdown([&completer](zx_status_t status) {
      ZX_ASSERT(status == ZX_OK);
      completer.Signal();
    });
    completer.Wait();

    memfs_.reset();
  }

 private:
  // Memfs' destructor blocks its thread while the shutdown happens on the dispatcher. If the
  // Admin.Shutdown call happens on the same dispatcher that is running memfs then that dispatcher
  // would require multiple threads. Memfs doesn't support running on a dispatcher with multiple
  // threads so it's given a separate thread.
  async::Loop loop_;

  std::mutex mutex_;
  OutgoingDirectory& outgoing_directory_ __TA_GUARDED(mutex_);

  std::unique_ptr<memfs::Memfs> memfs_ __TA_GUARDED(mutex_);
};

class StartupImpl final : public fidl::WireServer<fuchsia_fs_startup::Startup> {
 public:
  explicit StartupImpl(MemfsHandler& memfs_handler) : memfs_handler_(memfs_handler) {}

  void Start(StartRequestView request, StartCompleter::Sync& completer) final {
    if (auto status = memfs_handler_.Start(); status.is_error()) {
      completer.ReplyError(status.status_value());
    } else {
      completer.ReplySuccess();
    }
  }
  void Format(FormatRequestView request, FormatCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void Check(CheckRequestView request, CheckCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  MemfsHandler& memfs_handler_;
};

class AdminImpl : public fidl::WireServer<fuchsia_fs::Admin> {
 public:
  explicit AdminImpl(MemfsHandler& memfs_handler) : memfs_handler_(memfs_handler) {}

  void Shutdown(ShutdownCompleter::Sync& completer) final {
    memfs_handler_.Stop();
    completer.Reply();
  }

 private:
  MemfsHandler& memfs_handler_;
};

int main(int argc, char* argv[]) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  OutgoingDirectory outgoing_directory(dispatcher);
  MemfsHandler memfs_handler(outgoing_directory);

  if (zx::result status = outgoing_directory.ServeFromStartupInfo(); status.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << status.status_string();
    return EXIT_FAILURE;
  }

  auto status = outgoing_directory.AddUnmanagedProtocol<fuchsia_fs_startup::Startup>(
      [dispatcher, &memfs_handler](fidl::ServerEnd<fuchsia_fs_startup::Startup> server_end) {
        auto server = new StartupImpl(memfs_handler);
        fidl::BindServer(
            dispatcher, std::move(server_end), server,
            [](StartupImpl* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_fs_startup::Startup> server_end) { delete impl; });
      });
  if (status.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Startup protocol: " << status.status_string();
    return EXIT_FAILURE;
  }

  status = outgoing_directory.AddUnmanagedProtocol<fuchsia_fs::Admin>(
      [dispatcher, &memfs_handler](fidl::ServerEnd<fuchsia_fs::Admin> server_end) {
        auto server = new AdminImpl(memfs_handler);
        fidl::BindServer(dispatcher, std::move(server_end), server,
                         [](AdminImpl* impl, fidl::UnbindInfo info,
                            fidl::ServerEnd<fuchsia_fs::Admin> server_end) { delete impl; });
      });
  if (status.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Admin protocol: " << status.status_string();
    return EXIT_FAILURE;
  }

  loop.Run();
  return 0;
}
