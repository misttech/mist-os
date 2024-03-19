// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_REMOTE_DIR_H_
#define LIB_VFS_CPP_REMOTE_DIR_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/vfs/cpp/internal/node.h>
#include <lib/zx/channel.h>

namespace vfs {

// A remote directory holds a channel to a remotely hosted directory to which requests are delegated
// when opened.
//
// This class is designed to allow programs to publish remote filesystems as directories without
// requiring a separate "mount" step.  In effect, a remote directory is "mounted" at creation time.
//
// It is not possible for the client to detach the remote directory or to mount a new one in its
// place.
//
// This class is thread-safe.
class RemoteDir final : public internal::Node {
 public:
  // Binds to a remotely hosted directory channel via `remote_dir`. The channel must be valid and
  // must be compatible with the `fuchsia.io.Directory` protocol.
  explicit RemoteDir(zx::channel remote_dir)
      : internal::Node(CreateRemoteDir(std::move(remote_dir))) {}

  // Binds to a remotely hosted directory using the specified `dir`. The `dir` handle must be valid.
  explicit RemoteDir(fidl::InterfaceHandle<fuchsia::io::Directory> dir)
      : RemoteDir(dir.TakeChannel()) {}

  using internal::Node::Serve;

 private:
  static vfs_internal_node_t* CreateRemoteDir(zx::channel dir) {
    vfs_internal_node_t* remote;
    ZX_ASSERT(vfs_internal_remote_directory_create(dir.release(), &remote) == ZX_OK);
    return remote;
  }
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_REMOTE_DIR_H_
