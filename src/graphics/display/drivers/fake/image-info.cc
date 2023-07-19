// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/image-info.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/zx/vmo.h>

#include <cstddef>
#include <functional>

namespace fake_display {

CaptureImageInfo::CaptureImageInfo(IdType id, ImageMetadata metadata, zx::vmo vmo)
    : id_(id), metadata_(std::move(metadata)), vmo_(std::move(vmo)) {}

CaptureImageInfo::HashTable::KeyType CaptureImageInfo::GetKey() const { return id_; }

// static
size_t CaptureImageInfo::GetHash(HashTable::KeyType key) {
  return std::hash<HashTable::KeyType>()(key);
}

}  // namespace fake_display
