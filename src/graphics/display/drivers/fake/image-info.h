// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_IMAGE_INFO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_IMAGE_INFO_H_

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/zx/vmo.h>

#include <cstddef>
#include <memory>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>

#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"

namespace fake_display {

struct ImageMetadata {
  fuchsia_images2::PixelFormat pixel_format;
  fuchsia_sysmem2::CoherencyDomain coherency_domain;
};

class DisplayImageInfo : public fbl::SinglyLinkedListable<std::unique_ptr<DisplayImageInfo>> {
 public:
  using IdType = display::DriverImageId;
  using HashTable = fbl::HashTable<IdType, std::unique_ptr<DisplayImageInfo>>;

  DisplayImageInfo(IdType id, const ImageMetadata& metadata, zx::vmo vmo);
  ~DisplayImageInfo() = default;

  // Disallow copy and move.
  DisplayImageInfo(const DisplayImageInfo&) = delete;
  DisplayImageInfo& operator=(const DisplayImageInfo&) = delete;
  DisplayImageInfo(DisplayImageInfo&&) = delete;
  DisplayImageInfo& operator=(DisplayImageInfo&&) = delete;

  // Trait implementation for fbl::HashTable
  HashTable::KeyType GetKey() const;
  static size_t GetHash(HashTable::KeyType key);

  IdType id() const { return id_; }
  const ImageMetadata& metadata() const { return metadata_; }
  const zx::vmo& vmo() const { return vmo_; }

 private:
  IdType id_;
  ImageMetadata metadata_;
  zx::vmo vmo_;
};

class CaptureImageInfo : public fbl::SinglyLinkedListable<std::unique_ptr<CaptureImageInfo>> {
 public:
  using IdType = display::DriverCaptureImageId;
  using HashTable = fbl::HashTable<IdType, std::unique_ptr<CaptureImageInfo>>;

  CaptureImageInfo(IdType id, ImageMetadata metadata, zx::vmo vmo);
  ~CaptureImageInfo() = default;

  // Disallow copy and move.
  CaptureImageInfo(const CaptureImageInfo&) = delete;
  CaptureImageInfo& operator=(const CaptureImageInfo&) = delete;
  CaptureImageInfo(CaptureImageInfo&&) = delete;
  CaptureImageInfo& operator=(CaptureImageInfo&&) = delete;

  // Trait implementation for fbl::HashTable
  HashTable::KeyType GetKey() const;
  static size_t GetHash(HashTable::KeyType key);

  IdType id() const { return id_; }
  const ImageMetadata& metadata() const { return metadata_; }
  const zx::vmo& vmo() const { return vmo_; }

 private:
  IdType id_;
  ImageMetadata metadata_;
  zx::vmo vmo_;
};

}  // namespace fake_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_IMAGE_INFO_H_
