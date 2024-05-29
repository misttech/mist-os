// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_I2C_HID_I2C_HID_H_
#define SRC_UI_INPUT_DRIVERS_I2C_HID_I2C_HID_H_

#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <fidl/fuchsia.hardware.hidbus/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/device-protocol/i2c-channel.h>
#include <threads.h>

#include <memory>
#include <optional>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/devices/lib/acpi/client.h"

namespace i2c_hid {

// The I2c Hid Command codes.
constexpr uint8_t kResetCommand = 0x01;
constexpr uint8_t kGetReportCommand = 0x02;
constexpr uint8_t kSetReportCommand = 0x03;

// The i2c descriptor that describes the i2c's registers Ids.
// This is populated directly from an I2cRead call, so all
// of the values are in little endian.
struct I2cHidDesc {
  uint16_t wHIDDescLength;
  uint16_t bcdVersion;
  uint16_t wReportDescLength;
  uint16_t wReportDescRegister;
  uint16_t wInputRegister;
  uint16_t wMaxInputLength;
  uint16_t wOutputRegister;
  uint16_t wMaxOutputLength;
  uint16_t wCommandRegister;
  uint16_t wDataRegister;
  uint16_t wVendorID;
  uint16_t wProductID;
  uint16_t wVersionID;
  uint8_t RESERVED[4];
} __PACKED;

class I2cHidbus;
using DeviceType = ddk::Device<I2cHidbus, ddk::Initializable, ddk::Unbindable>;

class I2cHidbus : public DeviceType,
                  public ddk::EmptyProtocol<ZX_PROTOCOL_HIDBUS>,
                  public fidl::WireServer<fuchsia_hardware_hidbus::Hidbus> {
 public:
  explicit I2cHidbus(zx_device_t* device, acpi::Client client)
      : DeviceType(device),
        outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        acpi_client_(std::move(client)) {}
  ~I2cHidbus() = default;

  // Methods required by the ddk mixins.
  // fuchsia_hardware_hidbus methods.
  void Query(QueryCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void GetDescriptor(fuchsia_hardware_hidbus::wire::HidbusGetDescriptorRequest* request,
                     GetDescriptorCompleter::Sync& completer) override;
  void SetDescriptor(fuchsia_hardware_hidbus::wire::HidbusSetDescriptorRequest* request,
                     SetDescriptorCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetReport(fuchsia_hardware_hidbus::wire::HidbusGetReportRequest* request,
                 GetReportCompleter::Sync& completer) override;
  void SetReport(fuchsia_hardware_hidbus::wire::HidbusSetReportRequest* request,
                 SetReportCompleter::Sync& completer) override;
  // TODO(https://fxbug.dev/42109818): implement the rest of the HID protocol
  void GetIdle(fuchsia_hardware_hidbus::wire::HidbusGetIdleRequest* request,
               GetIdleCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetIdle(fuchsia_hardware_hidbus::wire::HidbusSetIdleRequest* request,
               SetIdleCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetProtocol(GetProtocolCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetProtocol(fuchsia_hardware_hidbus::wire::HidbusSetProtocolRequest* request,
                   SetProtocolCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  zx_status_t Bind(ddk::I2cChannel i2c);

  zx_status_t ReadI2cHidDesc(I2cHidDesc* hiddesc);

  // Must be called with i2c_lock held.
  void WaitForReadyLocked() __TA_REQUIRES(i2c_lock_);

  zx_status_t Reset(bool force) __TA_EXCLUDES(i2c_lock_);

  // For testing
  std::optional<fidl::ServerBinding<fuchsia_hardware_hidbus::Hidbus>>& binding() {
    return binding_;
  }

 private:
  void Shutdown();
  void Stop();

  component::OutgoingDirectory outgoing_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_hidbus::Hidbus>> binding_;
  async_dispatcher_t* dispatcher_;
  std::atomic_bool started_ = false;

  I2cHidDesc hiddesc_ = {};

  // Signaled when reset received.
  fbl::ConditionVariable i2c_reset_cnd_;

  std::optional<ddk::InitTxn> init_txn_;

  std::atomic_bool worker_thread_started_ = false;
  std::atomic_bool stop_worker_thread_ = false;
  thrd_t worker_thread_;
  zx::interrupt irq_;
  // The functions to be run in the worker thread. They are responsible for initializing the
  // driver and then reading Reports. If the I2c parent driver supports interrupts,
  // then |WorkerThreadIrq| will be used. Otherwise |WorkerThreadNoIrq| will be used and the
  // driver will poll periodically.
  int WorkerThreadIrq();
  int WorkerThreadNoIrq();

  fbl::Mutex i2c_lock_;
  ddk::I2cChannel i2c_ __TA_GUARDED(i2c_lock_);
  // True if reset-in-progress. Initalize as true so no work gets done until this is cleared.
  bool i2c_pending_reset_ __TA_GUARDED(i2c_lock_) = true;

  acpi::Client acpi_client_;
};

}  // namespace i2c_hid

#endif  // SRC_UI_INPUT_DRIVERS_I2C_HID_I2C_HID_H_
