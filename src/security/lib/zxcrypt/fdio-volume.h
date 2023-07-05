// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SECURITY_LIB_ZXCRYPT_FDIO_VOLUME_H_
#define SRC_SECURITY_LIB_ZXCRYPT_FDIO_VOLUME_H_

#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/result.h>

#include <memory>

#include "src/security/lib/fcrypto/secret.h"
#include "src/security/lib/zxcrypt/volume.h"

namespace zxcrypt {

// |zxcrypt::FdioVolume| is a zxcrypt volume with an underlying block device. It can be used on the
// host to prepare zxcrypt images, and is often more convenient for testing.
class __EXPORT FdioVolume final : public Volume {
 public:
  explicit FdioVolume(fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel);

  // Creates a new zxcrypt volume associated with the given block volume, |channel|. This will
  // format the block device as zxcrypt using the given |key|, which will be associated with key
  // slot 0. Note that |key| is not strengthened and MUST have cryptographic key length of at least
  // 128 bits.
  static zx::result<std::unique_ptr<FdioVolume>> Create(
      fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel, const crypto::Secret& key);

  // This is a convenience method that calls |Init| and then |Unlock|.
  static zx::result<std::unique_ptr<FdioVolume>> Unlock(
      fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel, const crypto::Secret& key,
      key_slot_t slot);

  // Returns a new volume object corresponding to the block device given by
  // |channel| and populated with the block and FVM information.
  static zx::result<std::unique_ptr<FdioVolume>> Init(
      fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel);

  // Opens a zxcrypt volume using the |key| corresponding to given key |slot|.
  zx_status_t Unlock(const crypto::Secret& key, key_slot_t slot);

  // Adds a given |key| to the given key |slot|.  This key can then be used to |Open| the
  // zxcrypt device.  This method can only be called if the volume belongs to libzxcrypt.
  zx_status_t Enroll(const crypto::Secret& key, key_slot_t slot);

  // Removes the root key in the given key |slot|.  This key can no longer be used to |Open| the
  // zxcrypt device.  This method can only be called if the volume belongs to libzxcrypt.
  zx_status_t Revoke(key_slot_t slot);

 private:
  friend class testing::TestDevice;

  // Retrieves the block and FVM information and adjusts it
  zx_status_t Init();

  zx_status_t GetBlockInfo(BlockInfo* out) override;
  zx_status_t GetFvmSliceSize(uint64_t* out) override;
  zx_status_t DoBlockFvmVsliceQuery(uint64_t vslice_start, SliceRegion ranges[MAX_SLICE_REGIONS],
                                    uint64_t* slice_count) override;
  zx_status_t DoBlockFvmExtend(uint64_t start_slice, uint64_t slice_count) override;

  // Reads a block from the current offset on the underlying device.
  zx_status_t Read() override;

  // Writes a block to the current offset on the underlying device.
  zx_status_t Write() override;

  // Flushes all pending writes to the underlying device.
  zx_status_t Flush() override;

  // The underlying block device.
  fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> device_;
};

}  // namespace zxcrypt

#endif  // SRC_SECURITY_LIB_ZXCRYPT_FDIO_VOLUME_H_
