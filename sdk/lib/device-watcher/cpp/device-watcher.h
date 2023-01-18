// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DEVICE_WATCHER_CPP_DEVICE_WATCHER_H_
#define LIB_DEVICE_WATCHER_CPP_DEVICE_WATCHER_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <memory>
#include <string_view>

namespace device_watcher {

// Waits for the relative |path| starting in the directory represented by |dir_fd| to appear,
// and opens it.
// This method does not take ownership of |dir_fd|.
//
// TODO(https://fxbug.dev/117188): Remove `timeout`.
zx::result<zx::channel> RecursiveWaitForFile(int dir_fd, const char* path,
                                             zx::duration timeout = zx::duration::infinite());

// Waits for the absolute |path| to appear, and opens it.
// NOTE: This only works for absolute paths,
// otherwise it will return ZX_ERR_NOT_SUPPORTED.
//
// TODO(https://fxbug.dev/117188): Remove `timeout`.
zx::result<zx::channel> RecursiveWaitForFile(const char* path,
                                             zx::duration timeout = zx::duration::infinite());

// Invokes |callback| on each entry in the directory, returning immediately after all entries have
// been processed. |callback| is passed the file name and a channel for the file's fuchsia.io.Node
// protocol. If |callback| returns a status other than ZX_OK, iteration terminates immediately, and
// the error status is returned. This function does not continue to watch the directory for newly
// created files.
// This method takes ownership of |fd|.
using FileCallback = fit::function<zx_status_t(std::string_view, zx::channel)>;
zx_status_t IterateDirectory(int fd, FileCallback callback);

// DirWatcher can be used to detect when a file has been removed from the filesystem.
//
// Example usage:
//
//   std::unique_ptr<DirWatcher> watcher;
//   zx_status_t status = DirWatcher::Create(dir_fd, &watcher);
//   ...
//   // Trigger removal of file here.
//   ...
//   status = watcher->WaitForRemoval(filename, deadline);
class DirWatcher {
 public:
  // Creates a new |DirWatcher| instance to watch the directory represented by |dir_fd|.
  // This method does not take ownership of |dir_fd|.
  static zx_status_t Create(int dir_fd, std::unique_ptr<DirWatcher>* out_dir_watcher);

  // Users should call Create instead. This is public for make_unique.
  explicit DirWatcher(fidl::ClientEnd<fuchsia_io::DirectoryWatcher> client)
      : client_(std::move(client)) {}

  // Returns ZX_OK if |filename| is removed from the directory before the given timeout elapses.
  // If no filename is specified, this will wait for any file in the directory to be removed.
  //
  // TODO(https://fxbug.dev/117188): Remove `timeout`.
  zx_status_t WaitForRemoval(std::string_view filename, zx::duration timeout);

 private:
  // A channel opened by a call to fuchsia.io.Directory.Watch, from which watch
  // events can be read.
  fidl::ClientEnd<fuchsia_io::DirectoryWatcher> client_;
};

}  // namespace device_watcher

#endif  // LIB_DEVICE_WATCHER_CPP_DEVICE_WATCHER_H_
