// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/fdio/namespace.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <zircon/process.h>
#include <zircon/status.h>

#include <fbl/ref_ptr.h>

#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/remote_dir.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

int main() {
  fdio_flat_namespace_t* ns;
  {
    zx_status_t status = fdio_ns_export_root(&ns);
    FX_CHECK(status == ZX_OK) << "failed to export root namespace: "
                              << zx_status_get_string(status);
  }
  fbl::RefPtr out_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  for (size_t i = 0; i < ns->count; ++i) {
    if (std::string_view{ns->path[i]} == "/svc") {
      zx_status_t status = out_dir->AddEntry(
          "svc_for_legacy_shell",
          fbl::MakeRefCounted<fs::RemoteDir>(fidl::ClientEnd<fuchsia_io::Directory>{
              zx::channel{std::exchange(ns->handle[i], ZX_HANDLE_INVALID)}}));
      FX_CHECK(status == ZX_OK) << "failed to add entry to outgoing dir: "
                                << zx_status_get_string(status);
    }
  }
  fdio_ns_free_flat_ns(ns);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fs::SynchronousVfs out_vfs(loop.dispatcher());

  {
    zx_status_t status = out_vfs.ServeDirectory(
        out_dir, fidl::ServerEnd<fuchsia_io::Directory>{
                     zx::channel{zx_take_startup_handle(PA_DIRECTORY_REQUEST)}});
    FX_CHECK(status == ZX_OK) << "failed to serve outgoing dir: " << zx_status_get_string(status);
  }

  {
    zx_status_t status = loop.Run();
    FX_CHECK(status == ZX_OK) << "failed to run loop: " << zx_status_get_string(status);
  }
  return 0;
}
