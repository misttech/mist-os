// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/memfs/mounted_memfs.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include "src/storage/memfs/vnode_dir.h"

zx::result<MountedMemfs> MountedMemfs::Create(async_dispatcher_t* dispatcher, const char* path) {
  zx::result result = memfs::Memfs::Create(dispatcher, "<tmp>");
  if (result.is_error()) {
    return result.take_error();
  }
  auto& [memfs, root] = result.value();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto& [client, server] = endpoints.value();

  if (zx_status_t status = memfs->ServeDirectory(std::move(root), std::move(server));
      status != ZX_OK) {
    return zx::error(status);
  }

  if (std::string_view{path}.empty()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  fdio_ns_t* ns;
  if (zx_status_t status = fdio_ns_get_installed(&ns); status != ZX_OK) {
    return zx::error(status);
  }
  if (zx_status_t status = fdio_ns_bind(ns, path, client.TakeChannel().release());
      status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(MountedMemfs(std::move(memfs), ns, path));
}
