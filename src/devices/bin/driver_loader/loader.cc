// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_loader/loader.h"

namespace driver_loader {

// static
std::unique_ptr<Loader> Loader::Create() {
  // TODO(https://fxbug.dev/341966687): retrieve the loader stub vmo.
  return std::make_unique<Loader>();
}

zx_status_t Loader::Start(zx::process process, zx::thread thread, zx::vmar root_vmar,
                          zx::vmo exec_vmo, zx::vmo vdso_vmo) {
  // TODO(https://fxbug.dev/341966687): implement this.
  return ZX_ERR_NOT_SUPPORTED;
}

}  // namespace driver_loader
