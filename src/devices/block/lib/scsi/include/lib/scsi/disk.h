// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_DISK_H_
#define SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_DISK_H_

#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/scsi/controller.h>
#include <stdint.h>

#include <fbl/string_printf.h>

namespace scsi {

struct DiskOp {
  void Complete(zx_status_t status) { completion_cb(cookie, status, &op); }

  block_op_t op;
  block_impl_queue_callback completion_cb;
  void* cookie;
};

struct DiskOptions {
  static DiskOptions Default() {
    return DiskOptions(/*check_unmap_support=*/false, /*use_mode_sense_6*/ true,
                       /*use_read_write_12*/ false);
  }

  explicit DiskOptions(bool check_unmap_support, bool use_mode_sense_6, bool use_read_write_12)
      : check_unmap_support(check_unmap_support),
        use_mode_sense_6(use_mode_sense_6),
        use_read_write_12(use_read_write_12) {}
  DiskOptions() = delete;

  bool check_unmap_support;
  bool use_mode_sense_6;
  bool use_read_write_12;
};

// |Disk| represents a single SCSI direct access block device.
// |Disk| bridges between the Zircon block protocol and SCSI commands/responses.
class Disk : public ddk::BlockImplProtocol<Disk> {
 public:
  // Public so that we can use make_unique.
  // Clients should use Disk::Bind().
  Disk(Controller* controller, uint8_t target, uint16_t lun, DiskOptions disk_options)
      : controller_(controller), target_(target), lun_(lun), disk_options_(disk_options) {}

  // Create a Disk at a specific target/lun.
  // |controller| is a pointer to the scsi::Controller this disk is attached to.
  // |controller| must outlast Disk.
  // This disk does not take ownership of or any references on |controller|.
  // A |max_transfer_bytes| value of fuchsia_hardware_block::wire::kMaxTransferUnbounded implies
  // there is no limit on the transfer size.
  // Returns a Disk* to allow for removal of removable media disks.
  static zx::result<std::unique_ptr<Disk>> Bind(Controller* controller, uint8_t target,
                                                uint16_t lun, uint32_t max_transfer_bytes,
                                                DiskOptions disk_options);

  // Remove this disk device.
  void RemoveDevice() {
    auto result = node_controller_->Remove();
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to call Remove on node controller.");
    }
  }

  fbl::String DiskName() const { return fbl::StringPrintf("scsi-disk-%u-%u", target_, lun_); }

  // ddk::BlockImplProtocol functions.
  void BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out);
  void BlockImplQueue(block_op_t* operation, block_impl_queue_callback completion_cb, void* cookie);

  uint8_t target() const { return target_; }
  uint16_t lun() const { return lun_; }

  bool removable() const { return removable_; }
  bool dpo_fua_available() const { return dpo_fua_available_; }
  bool write_protected() const { return write_protected_; }
  bool write_cache_enabled() const { return write_cache_enabled_; }
  uint64_t block_count() const { return block_count_; }
  uint32_t block_size_bytes() const { return block_size_bytes_; }
  uint32_t max_transfer_bytes() const { return max_transfer_bytes_; }

  Disk(const Disk&) = delete;
  Disk& operator=(const Disk&) = delete;

  // for test
  DiskOptions& GetDiskOptions() { return disk_options_; }

 private:
  zx_status_t AddDisk(uint32_t max_transfer_bytes);

  fdf::Logger& logger();

  Controller* const controller_;
  const uint8_t target_;
  const uint16_t lun_;
  uint32_t max_transfer_bytes_;
  uint32_t max_transfer_blocks_;

  bool removable_;
  bool dpo_fua_available_;
  bool write_protected_;
  bool write_cache_enabled_;

  bool unmap_command_supported_ = false;

  uint64_t block_count_;
  uint32_t block_size_bytes_;

  DiskOptions disk_options_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  compat::BanjoServer block_impl_server_{ZX_PROTOCOL_BLOCK_IMPL, this, &block_impl_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace scsi

#endif  // SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_DISK_H_
