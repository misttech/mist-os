// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_
#define SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_

#include <lib/dma-buffer/buffer.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/scsi/block-device.h>
#include <lib/scsi/controller.h>
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
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <virtio/scsi.h>

namespace virtio {

constexpr int MAX_IOS = 16;

class ScsiDriver;

class ScsiDevice : public virtio::Device {
 public:
  enum Queue {
    CONTROL = 0,
    EVENT = 1,
    REQUEST = 2,
  };

  ScsiDevice(ScsiDriver* scsi_driver, zx::bti bti, std::unique_ptr<Backend> backend)
      : virtio::Device(std::move(bti), std::move(backend)), scsi_driver_(scsi_driver) {}

  // virtio::Device overrides
  zx_status_t Init() override;
  // Invoked for most device interrupts.
  void IrqRingUpdate() override;
  // Invoked on config change interrupts.
  void IrqConfigChange() override {}
  const char* tag() const override { return "virtio-scsi"; }

  static void FillLUNStructure(struct virtio_scsi_req_cmd* req, uint8_t target, uint16_t lun);

  void QueueCommand(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                    zx::unowned_vmo data_vmo, zx_off_t vmo_offset_bytes, size_t transfer_bytes,
                    void (*cb)(void*, zx_status_t), void* cookie, void* data, bool vmar_mapped,
                    std::optional<zx::vmo> trim_data_vmo = std::nullopt);

  zx::result<> AllocatePages(zx::vmo& vmo, fzl::VmoMapper& mapper, size_t size);

  zx_status_t ProbeLuns();

 private:
  ScsiDriver* const scsi_driver_;

  // Latched copy of virtio-scsi device configuration.
  struct virtio_scsi_config config_ TA_GUARDED(lock_) = {};

  struct scsi_io_slot {
    zx::unowned_vmo data_vmo;
    zx_off_t vmo_offset_bytes;
    size_t transfer_bytes;
    bool is_write;
    void* data;
    bool vmar_mapped;
    std::unique_ptr<dma_buffer::ContiguousBuffer> request_buffer;
    bool avail;
    vring_desc* tail_desc;
    void* cookie;
    void (*callback)(void* cookie, zx_status_t status);
    void* data_in_region;
    dma_buffer::ContiguousBuffer* request_buffers;
    struct virtio_scsi_resp_cmd* response;
    // Sustains the lifetime of the trim data while it is being used.
    std::optional<zx::vmo> trim_data_vmo;
  };
  scsi_io_slot* GetIO() TA_REQ(lock_);
  void FreeIO(scsi_io_slot* io_slot) TA_REQ(lock_);
  size_t request_buffers_size_;
  scsi_io_slot scsi_io_slot_table_[MAX_IOS] TA_GUARDED(lock_) = {};

  Ring control_ring_ TA_GUARDED(lock_){this};
  Ring request_queue_{this};

  // Synchronizes virtio rings and worker thread control.
  fbl::Mutex lock_;

  // We use the condvar to control the number of IO's in flight
  // as well as to wait for descs to become available.
  fbl::ConditionVariable ioslot_cv_ __TA_GUARDED(lock_);
  fbl::ConditionVariable desc_cv_ __TA_GUARDED(lock_);
  uint32_t active_ios_ __TA_GUARDED(lock_);
  uint64_t scsi_transport_tag_ __TA_GUARDED(lock_);
};

class ScsiDriver : public fdf::DriverBase, public scsi::Controller {
 public:
  static constexpr char kDriverName[] = "virtio-scsi";

  ScsiDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // scsi::Controller overrides
  fidl::WireSyncClient<fuchsia_driver_framework::Node>& root_node() override { return root_node_; }
  std::string_view driver_name() const override { return name(); }
  const std::shared_ptr<fdf::Namespace>& driver_incoming() const override { return incoming(); }
  std::shared_ptr<fdf::OutgoingDirectory>& driver_outgoing() override { return outgoing(); }
  const std::optional<std::string>& driver_node_name() const override { return node_name(); }
  fdf::Logger& driver_logger() override { return logger(); }
  size_t BlockOpSize() override {
    // No additional metadata required for each command transaction.
    return sizeof(scsi::DeviceOp);
  }
  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override;
  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, scsi::DeviceOp* device_op,
                           iovec data) override;

 private:
  std::unique_ptr<ScsiDevice> scsi_device_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> root_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;
};

}  // namespace virtio

#endif  // SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_SCSI_H_
