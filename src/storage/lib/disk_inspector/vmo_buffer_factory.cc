// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/disk_inspector/vmo_buffer_factory.h"

#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstddef>
#include <memory>
#include <utility>

#include <storage/buffer/block_buffer.h>
#include <storage/buffer/vmo_buffer.h>

namespace disk_inspector {

zx::result<std::unique_ptr<storage::BlockBuffer>> VmoBufferFactory::CreateBuffer(
    size_t capacity) const {
  auto buffer = std::make_unique<storage::VmoBuffer>();
  zx_status_t status = buffer->Initialize(registry_, capacity, block_size_, "factory-vmo-buffer");
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(buffer));
}

}  // namespace disk_inspector
