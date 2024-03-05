// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UMS_FUNCTION_UMS_FUNCTION_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UMS_FUNCTION_UMS_FUNCTION_H_

#include <fuchsia/hardware/usb/function/cpp/banjo.h>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <usb/request-cpp.h>
#include <usb/ums.h>
#include <usb/usb-request.h>

namespace ums {

class UmsFunction;
using DeviceType = ddk::Device<UmsFunction, ddk::Initializable, ddk::Unbindable>;
class UmsFunction : public DeviceType, public ddk::UsbFunctionInterfaceProtocol<UmsFunction> {
 public:
  static constexpr char kDriverName[] = "usb-ums-function";
  static constexpr uint32_t kBlockSize = 512;
  static constexpr size_t kStorageSize = 4L * 1024L * 1024L * 1024L;
  static constexpr uint64_t kBlockCount = kStorageSize / kBlockSize;
  static constexpr size_t kDataReqSize = 16384;
  static constexpr uint16_t kBulkMaxPacket = 512;

  UmsFunction(zx_device_t* parent, ddk::UsbFunctionProtocolClient function)
      : DeviceType(parent), function_(function) {}
  ~UmsFunction() = default;

  static zx_status_t Bind(void* ctx, zx_device_t* parent);
  zx_status_t AddDevice(zx_device_t* parent);

  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  // ddk::UsbFunctionInterfaceProtocol implementations.
  size_t UsbFunctionInterfaceGetDescriptorsSize();
  void UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer, size_t descriptors_size,
                                          size_t* out_descriptors_actual);
  zx_status_t UsbFunctionInterfaceControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                          size_t write_size, uint8_t* out_read_buffer,
                                          size_t read_size, size_t* out_read_actual);
  zx_status_t UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed);
  zx_status_t UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting);

 private:
  enum DataState {
    DATA_STATE_NONE,
    DATA_STATE_READ,
    DATA_STATE_WRITE,
    DATA_STATE_UNMAP,
    DATA_STATE_FAILED
  };

  void RequestQueue(usb::Request<>* req, const usb_request_complete_callback_t* completion);
  static void CompletionCallback(void* ctx, usb_request_t* req);

  // Main driver initialization.
  zx_status_t Init();

  void QueueData(usb::Request<>* req);
  void QueueCsw(uint8_t status);
  void ContinueTransfer();
  void StartTransfer(DataState state, uint32_t transfer_bytes, uint64_t lba = 0);

  void HandleInquiry(ums_cbw_t* cbw);
  void HandleTestUnitReady(ums_cbw_t* cbw);
  void HandleRequestSense(ums_cbw_t* cbw);
  void HandleReadCapacity10(ums_cbw_t* cbw);
  void HandleReadCapacity16(ums_cbw_t* cbw);
  void HandleModeSense6(ums_cbw_t* cbw);
  void HandleRead10(ums_cbw_t* cbw);
  void HandleRead12(ums_cbw_t* cbw);
  void HandleRead16(ums_cbw_t* cbw);
  void HandleWrite10(ums_cbw_t* cbw);
  void HandleWrite12(ums_cbw_t* cbw);
  void HandleWrite16(ums_cbw_t* cbw);
  void HandleUnmap(ums_cbw_t* cbw);

  void HandleCbw(ums_cbw_t* cbw);
  void CbwComplete(usb::Request<>* req);
  void DataComplete(usb::Request<>* req);

  int WorkerLoop();

  ddk::UsbFunctionProtocolClient function_;

  std::optional<usb::Request<>> cbw_req_;
  bool cbw_req_complete_ = false;
  std::optional<usb::Request<>> data_req_;
  bool data_req_complete_ = false;
  std::optional<usb::Request<>> csw_req_;
  bool csw_req_complete_ = false;

  // vmo for backing storage
  static zx::vmo vmo_;
  void* storage_;

  // command we are currently handling
  ums_cbw_t current_cbw_ = {};
  // data transferred for the current command
  uint32_t data_length_ = 0;

  // state for data transfers
  DataState data_state_;
  // state for reads and writes
  zx_off_t data_offset_ = 0;
  size_t data_remaining_ = 0;

  uint8_t bulk_out_addr_;
  uint8_t bulk_in_addr_;
  size_t parent_req_size_;
  thrd_t thread_;
  bool active_;
  fbl::Mutex mtx_;
  fbl::ConditionVariable condvar_ __TA_GUARDED(mtx_);
  std::atomic_int pending_request_count_;
};

}  // namespace ums

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UMS_FUNCTION_UMS_FUNCTION_H_
