// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include <fbl/ref_ptr.h>

#include "src/developer/vsock-sshd-host/data_dir.h"
#include "src/developer/vsock-sshd-host/service.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace {
const uint16_t kPort = 22;

class DevNullVnode : public fs::Vnode {
 public:
  explicit DevNullVnode() = default;

  zx_status_t Read(void* data, size_t len, size_t off, size_t* out_actual) override {
    *out_actual = 0;
    return ZX_OK;
  }

  zx_status_t Write(const void* data, size_t len, size_t off, size_t* out_actual) override {
    *out_actual = len;
    return ZX_OK;
  }

  zx_status_t Truncate(size_t len) override { return ZX_OK; }

  zx_status_t GetAttributes(fs::VnodeAttributes* a) override {
    a->mode = V_TYPE_CDEV | V_IRUSR | V_IWUSR;
    a->content_size = 0;
    a->link_count = 1;
    return ZX_OK;
  }

  fuchsia_io::NodeProtocolKinds GetProtocols() const override {
    return fuchsia_io::NodeProtocolKinds::kFile;
  }
};

}  // namespace

int main(int argc, const char** argv) {
  fuchsia_logging::SetTags({"sshd-host"});

  FX_SLOG(INFO, "sshd-host starting up");

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "data");
  if (result.is_error()) {
    FX_LOGS(FATAL) << "Failed to create memfs with error " << result.status_string();
  }
  auto& [memfs, data_dir] = result.value();

  {
    auto result = BuildDataDir(loop, memfs.get(), data_dir);
    if (result.is_error()) {
      FX_LOGS(FATAL) << "Failed to create build data dir with error " << result.status_string();
    }
  }

  auto root = fbl::MakeRefCounted<fs::PseudoDir>();
  root->AddEntry("data", std::move(data_dir));
  auto dev = fbl::MakeRefCounted<fs::PseudoDir>();
  dev->AddEntry("null", fbl::MakeRefCounted<DevNullVnode>());
  root->AddEntry("dev", std::move(dev));

  // Serve outgoing directory
  auto outgoing_request = fidl::ServerEnd<fuchsia_io::Directory>(
      zx::channel((zx_take_startup_handle(PA_DIRECTORY_REQUEST))));
  if (zx_status_t status = memfs->ServeDirectory(std::move(root), std::move(outgoing_request));
      status != ZX_OK) {
    FX_LOGS(FATAL) << "Failed to host outgoing directory " << status;
  }

  uint16_t port = kPort;
  if (argc > 1) {
    int arg = atoi(argv[1]);
    if (arg <= 0) {
      FX_SLOG(ERROR, "Invalid port", FX_KV("argv[1]", argv[1]));
      return -1;
    }
    port = static_cast<uint16_t>(arg);
  }
  sshd_host::Service service(loop.dispatcher(), port);

  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    FX_SLOG(FATAL, "Failed to run loop", FX_KV("status", zx_status_get_string(status)));
  }

  return 0;
}
