// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_
#define SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_

#include <lib/scsi/controller.h>
#include <lib/scsi/disk.h>
#include <lib/sync/completion.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stdlib.h>
#include <sys/uio.h>
#include <zircon/compiler.h>

#include <atomic>
#include <memory>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <virtio/scsi.h>

namespace virtio {

constexpr int MAX_IOS = 16;

class ScsiDevice : public virtio::Device, public scsi::Controller, public ddk::Device<ScsiDevice> {
 public:
  enum Queue {
    CONTROL = 0,
    EVENT = 1,
    REQUEST = 2,
  };

  ScsiDevice(zx_device_t* device, zx::bti bti, std::unique_ptr<Backend> backend)
      : virtio::Device(std::move(bti), std::move(backend)),
        ddk::Device<ScsiDevice>(device),
        device_(device) {}

  // virtio::Device overrides
  zx_status_t Init() override;
  void DdkRelease();
  // Invoked for most device interrupts.
  virtual void IrqRingUpdate() override;
  // Invoked on config change interrupts.
  void IrqConfigChange() override {}

  // scsi::Controller overrides
  size_t BlockOpSize() override {
    // No additional metadata required for each command transaction.
    return sizeof(scsi::DiskOp);
  }
  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override;
  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, scsi::DiskOp* disk_op) override;

  const char* tag() const override { return "virtio-scsi"; }

  static void FillLUNStructure(struct virtio_scsi_req_cmd* req, uint8_t target, uint16_t lun);

 private:
  void QueueCommand(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                    zx::unowned_vmo data_vmo, zx_off_t vmo_offset_bytes, size_t transfer_bytes,
                    void (*cb)(void*, zx_status_t), void* cookie, void* data, bool vmar_mapped);

  zx_status_t WorkerThread();

  // Latched copy of virtio-scsi device configuration.
  struct virtio_scsi_config config_ TA_GUARDED(lock_) = {};

  struct scsi_io_slot {
    zx::unowned_vmo data_vmo;
    zx_off_t vmo_offset_bytes;
    size_t transfer_bytes;
    bool is_write;
    void* data;
    bool vmar_mapped;
    io_buffer_t request_buffer;
    bool avail;
    vring_desc* tail_desc;
    void* cookie;
    void (*callback)(void* cookie, zx_status_t status);
    void* data_in_region;
    io_buffer_t* request_buffers;
    struct virtio_scsi_resp_cmd* response;
  };
  scsi_io_slot* GetIO() TA_REQ(lock_);
  void FreeIO(scsi_io_slot* io_slot) TA_REQ(lock_);
  size_t request_buffers_size_;
  scsi_io_slot scsi_io_slot_table_[MAX_IOS] TA_GUARDED(lock_) = {};

  zx_device_t* device_ = nullptr;

  Ring control_ring_ TA_GUARDED(lock_) = {this};
  Ring request_queue_ = {this};

  thrd_t worker_thread_;
  bool worker_thread_should_exit_ TA_GUARDED(lock_) = {};

  // Synchronizes virtio rings and worker thread control.
  fbl::Mutex lock_;

  // We use the condvar to control the number of IO's in flight
  // as well as to wait for descs to become available.
  fbl::ConditionVariable ioslot_cv_ __TA_GUARDED(lock_);
  fbl::ConditionVariable desc_cv_ __TA_GUARDED(lock_);
  uint32_t active_ios_ __TA_GUARDED(lock_);
  uint64_t scsi_transport_tag_ __TA_GUARDED(lock_);
};

}  // namespace virtio

#endif  // SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_
