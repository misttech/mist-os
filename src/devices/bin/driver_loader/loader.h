// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_
#define SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_

#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <memory>

namespace driver_loader {

class Loader {
 public:
  static std::unique_ptr<Loader> Create();

  // Use |Create| instead. This is public for |std::make_unique|.
  // TODO(https://fxbug.dev/341966687): construct the remote abi stub.
  Loader() {}

  // Loads the executable |exec| with |vdso| into |process|, and begins the execution on |thread|.
  zx_status_t Start(zx::process process, zx::thread thread, zx::vmar root_vmar, zx::vmo exec,
                    zx::vmo vdso);
};

}  // namespace driver_loader

#endif  // SRC_DEVICES_BIN_DRIVER_LOADER_LOADER_H_
