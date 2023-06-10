// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/svc/outgoing.h>
#include <lib/zx/channel.h>

#include "src/lib/storage/vfs/cpp/remote_dir.h"
#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"

int main(int argc, char* argv[]) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  if (result.is_error()) {
    fprintf(stderr, "Failed to create memfs: %s\n", result.status_string());
    return -1;
  }
  auto& [memfs, root] = result.value();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    fprintf(stderr, "Failed to create memfs endpoints: %s\n", endpoints.status_string());
    return -1;
  }
  auto& [memfs_dir, server] = endpoints.value();

  if (zx_status_t status = memfs->ServeDirectory(std::move(root), std::move(server));
      status != ZX_OK) {
    fprintf(stderr, "Failed to server memfs directory: %s\n", zx_status_get_string(status));
    return -1;
  }

  svc::Outgoing outgoing(loop.dispatcher());

  for (fbl::String name : {"restricted", "unrestricted"}) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      fprintf(stderr, "Failed to create endpoints: %s\n", endpoints.status_string());
      return -1;
    }
    auto& [client, node_server] = endpoints.value();
    fidl::ServerEnd<fuchsia_io::Node> server(node_server.TakeChannel());
    if (fidl::Status result =
            fidl::WireCall(memfs_dir)->Clone(fuchsia_io::wire::OpenFlags::kRightReadable |
                                                 fuchsia_io::wire::OpenFlags::kRightWritable,
                                             std::move(server));
        !result.ok()) {
      fprintf(stderr, "Failed to clone memfs dir: %s\n", result.FormatDescription().c_str());
      return -1;
    }
    if (zx_status_t status = outgoing.root_dir()->AddEntry(
            name, fbl::MakeRefCounted<fs::RemoteDir>(std::move(client)));
        status != ZX_OK) {
      fprintf(stderr, "Failed to add outgoing entry: %s\n", zx_status_get_string(status));
      return -1;
    }
  }

  if (zx_status_t status = outgoing.ServeFromStartupInfo(); status != ZX_OK) {
    fprintf(stderr, "Failed to serve outgoing dir: %s\n", zx_status_get_string(status));
    return -1;
  }

  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    fprintf(stderr, "Failed to run loop: %s\n", zx_status_get_string(status));
    return -1;
  }

  return 0;
}
