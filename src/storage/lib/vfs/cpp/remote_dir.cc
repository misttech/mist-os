// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/remote_dir.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include <utility>

#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace fio = fuchsia_io;

namespace fs {

RemoteDir::RemoteDir(fidl::ClientEnd<fio::Directory> remote_dir_client)
    : remote_client_(std::move(remote_dir_client)) {
  ZX_DEBUG_ASSERT(remote_client_);
}

RemoteDir::~RemoteDir() = default;

fuchsia_io::NodeProtocolKinds RemoteDir::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kDirectory;
}

zx::result<fs::VnodeAttributes> RemoteDir::GetAttributes() const {
  return zx::ok(fs::VnodeAttributes{
      .mode = V_TYPE_DIR | V_IRUSR,
  });
}

bool RemoteDir::IsRemote() const { return true; }

zx_status_t RemoteDir::OpenRemote(fio::OpenFlags flags, fio::ModeType mode, fidl::StringView path,
                                  fidl::ServerEnd<fio::Node> object) const {
  FS_PRETTY_TRACE_DEBUG("RemoteDir::OpenRemote: path='", path, "' flags=", flags);
  return fidl::WireCall(remote_client_)->Open(flags, mode, path, std::move(object)).status();
}

}  // namespace fs
