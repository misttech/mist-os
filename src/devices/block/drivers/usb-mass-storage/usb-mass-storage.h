// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_USB_MASS_STORAGE_USB_MASS_STORAGE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_USB_MASS_STORAGE_USB_MASS_STORAGE_H_

#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/usb/c/banjo.h>
#include <inttypes.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/scsi/controller.h>
#include <lib/scsi/disk.h>
#include <lib/sync/completion.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/assert.h>
#include <zircon/listnode.h>

#include <atomic>
#include <memory>
#include <mutex>

#include <fbl/array.h>
#include <fbl/condition_variable.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <usb/ums.h>
#include <usb/usb-request.h>
#include <usb/usb.h>

namespace ums {

// Abstract waiter class for waiting on a sync_completion_t.
// This is necessary to allow injection of a timer by a test
// into the UsbMassStorageDevice class, allowing for a simulated clock.
class WaiterInterface : public fbl::RefCounted<WaiterInterface> {
 public:
  virtual zx_status_t Wait(sync_completion_t* completion, zx_duration_t duration) = 0;
  virtual ~WaiterInterface() = default;
};

// struct representing a block device for a logical unit
struct Transaction {
  scsi::DiskOp disk_op;

  // UsbMassStorageDevice::ExecuteCommandAsync() checks that the incoming CDB's size does not exceed
  // this buffer's.
  uint8_t cdb_buffer[16];
  uint8_t cdb_length;
  uint8_t lun;
  uint32_t block_size_bytes;

  // Currently, data_buffer is only used by the UNMAP command and has a maximum size of 24 byte.
  uint8_t data_buffer[24];
  zx::vmo data_vmo;

  list_node_t node;
};

struct UsbRequestContext {
  usb_request_complete_callback_t completion;
};

class UsbMassStorageDevice : public fdf::DriverBase, public scsi::Controller {
 public:
  static constexpr char kDriverName[] = "ums";

  UsbMassStorageDevice(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}
  ~UsbMassStorageDevice() override = default;

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // scsi::Controller
  fidl::WireSyncClient<fuchsia_driver_framework::Node>& root_node() override { return root_node_; }
  std::string_view driver_name() const override { return name(); }
  const std::shared_ptr<fdf::Namespace>& driver_incoming() const override { return incoming(); }
  std::shared_ptr<fdf::OutgoingDirectory>& driver_outgoing() override { return outgoing(); }
  const std::optional<std::string>& driver_node_name() const override { return node_name(); }
  fdf::Logger& driver_logger() override { return logger(); }
  size_t BlockOpSize() override { return sizeof(Transaction); }
  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override;
  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, scsi::DiskOp* disk_op, iovec data) override;

  // Performs the object initialization.
  zx_status_t Init();

  // Visible for testing.
  const std::vector<std::unique_ptr<scsi::Disk>>& block_devs() const { return block_devs_; }
  void set_waiter(fbl::RefPtr<WaiterInterface> waiter) { waiter_ = std::move(waiter); }

  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbMassStorageDevice);

 private:
  zx_status_t Reset();

  // Sends a Command Block Wrapper (command portion of request)
  // to a USB mass storage device.
  zx_status_t SendCbw(uint8_t lun, uint32_t transfer_length, uint8_t flags, uint8_t command_len,
                      void* command);

  // Reads a Command Status Wrapper from a USB mass storage device
  // and validates that the command index in the response matches the index
  // in the previous request.
  zx_status_t ReadCsw(uint32_t* out_residue, bool retry = false);

  // Validates the command index and signature of a command status wrapper.
  csw_status_t VerifyCsw(usb_request_t* csw_request, uint32_t* out_residue);

  zx_status_t ReadSync(size_t transfer_length);

  zx_status_t DataTransfer(zx_handle_t vmo_handle, zx_off_t offset, size_t length,
                           uint8_t ep_address);

  zx_status_t DoTransaction(Transaction* txn, uint8_t flags, uint8_t ep_address,
                            const std::string& action);

  zx_status_t CheckLunsReady();

  void WorkerLoop();

  void RequestQueue(usb_request_t* request, const usb_request_complete_callback_t* completion);

  zx::result<> AllocatePages(zx::vmo& vmo, fzl::VmoMapper& mapper, size_t size);

  usb::UsbDevice usb_;

  uint32_t tag_send_;  // next tag to send in CBW

  uint32_t tag_receive_;  // next tag we expect to receive in CSW

  uint8_t max_lun_;  // index of last logical unit

  uint32_t max_transfer_bytes_;  // maximum transfer size reported by usb_get_max_transfer_size()

  uint8_t interface_number_;

  // Dispatcher for processing queued block requests.
  fdf::Dispatcher worker_dispatcher_;
  // Signaled when worker_dispatcher_ is shut down.
  libsync::Completion worker_shutdown_completion_;

  uint8_t bulk_in_addr_;

  uint8_t bulk_out_addr_;

  size_t bulk_in_max_packet_;

  size_t bulk_out_max_packet_;

  usb_request_t* cbw_req_;

  usb_request_t* data_req_;

  usb_request_t* csw_req_;

  usb_request_t* data_transfer_req_;  // for use in DataTransfer

  size_t parent_req_size_;

  std::atomic_size_t pending_requests_ = 0;

  fbl::RefPtr<WaiterInterface> waiter_;

  std::atomic_bool dead_ = false;

  // list of queued transactions
  list_node_t queued_txns_;

  sync_completion_t txn_completion_;  // signals WorkerLoop when new txns are available
                                      // and when device is dead
  std::mutex txn_lock_;               // protects queued_txns, txn_completion and dead
  std::mutex luns_lock_;              // Synchronizes the checking of whether LUNs are ready.

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> root_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  std::vector<std::unique_ptr<scsi::Disk>> block_devs_;
};

}  // namespace ums

#endif  // SRC_DEVICES_BLOCK_DRIVERS_USB_MASS_STORAGE_USB_MASS_STORAGE_H_
