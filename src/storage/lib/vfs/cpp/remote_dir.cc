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

fio::NodeProtocolKinds RemoteDir::GetProtocols() const {
  return fio::NodeProtocolKinds::kDirectory;
}

bool RemoteDir::IsRemote() const { return true; }

void RemoteDir::OpenRemote(fio::OpenFlags flags, fio::ModeType mode, fidl::StringView path,
                           fidl::ServerEnd<fio::Node> object) const {
  // We consume |object| when making the wire call to the remote end, so on failure there isn't
  // anywhere for us to propagate the error.
  [[maybe_unused]] auto status =
      fidl::WireCall(remote_client_)->Open(flags, mode, path, std::move(object));
  FS_PRETTY_TRACE_DEBUG("RemoteDir::OpenRemote: path='", path, "', flags=", flags,
                        ", response=", status.FormatDescription());
}

void RemoteDir::OpenRemote(fuchsia_io::wire::Directory2Open2Request request) const {
  // We consume the |request| channel when making the wire call to the remote end, so on failure
  // there isn't anywhere for us to propagate the error.
  [[maybe_unused]] auto status =
      fidl::WireCall(remote_client_)
          ->Open2(request.path, request.protocols, std::move(request.object_request));
  FS_PRETTY_TRACE_DEBUG("RemoteDir::OpenRemote: path='", request.path,
                        "', protocols=", request.protocols,
                        ", response=", status.FormatDescription());
}

}  // namespace fs
