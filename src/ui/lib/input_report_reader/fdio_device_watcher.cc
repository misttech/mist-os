// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/input_report_reader/fdio_device_watcher.h"

#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>

#include "src/lib/fsl/io/device_watcher.h"

namespace ui_input {

FdioDeviceWatcher::FdioDeviceWatcher(std::string directory_path)
    : directory_path_(std::move(directory_path)) {}
FdioDeviceWatcher::~FdioDeviceWatcher() = default;

void FdioDeviceWatcher::Watch(ExistsCallback callback) {
  FX_DCHECK(!watch_);
  watch_ = fsl::DeviceWatcher::Create(
      directory_path_, [callback = std::move(callback)](int dir_fd, const std::string& filename) {
        zx::channel client, server;
        if (zx_status_t status = zx::channel::create(0, &client, &server); status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "failed to create channel";
          return;
        }
        fdio_cpp::UnownedFdioCaller caller(dir_fd);
        if (zx_status_t status = fdio_service_connect_at(caller.borrow_channel(), filename.c_str(),
                                                         server.release());
            status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "failed to connect to " << filename;
          return;
        }
        callback(std::move(client));
      });
}

}  // namespace ui_input
