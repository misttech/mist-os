// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_H_

#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <fuchsia/hardware/serialimpl/async/cpp/banjo.h>
#include <lib/async/cpp/irq.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <fbl/mutex.h>

namespace serial {

namespace internal {

class DriverTransportReadOperation {
  using ReadCompleter = fidl::internal::WireCompleter<fuchsia_hardware_serialimpl::Device::Read>;

 public:
  DriverTransportReadOperation(fdf::Arena arena, ReadCompleter::Async completer)
      : arena_(std::move(arena)), completer_(std::move(completer)) {}

  fit::closure MakeCallback(zx_status_t status, void* buf, size_t len);

 private:
  fdf::Arena arena_;
  ReadCompleter::Async completer_;
};

class BanjoReadOperation {
 public:
  BanjoReadOperation(serial_impl_async_read_async_callback callback, void* cookie)
      : callback_(callback), cookie_(cookie) {}

  fit::closure MakeCallback(zx_status_t status, void* buf, size_t len);

 private:
  serial_impl_async_read_async_callback callback_;
  void* cookie_;
};

class DriverTransportWriteOperation {
  using WriteCompleter = fidl::internal::WireCompleter<fuchsia_hardware_serialimpl::Device::Write>;

 public:
  DriverTransportWriteOperation(fdf::Arena arena, WriteCompleter::Async completer)
      : arena_(std::move(arena)), completer_(std::move(completer)) {}

  fit::closure MakeCallback(zx_status_t status);

 private:
  fdf::Arena arena_;
  WriteCompleter::Async completer_;
};

class BanjoWriteOperation {
 public:
  BanjoWriteOperation(serial_impl_async_write_async_callback callback, void* cookie)
      : callback_(callback), cookie_(cookie) {}

  fit::closure MakeCallback(zx_status_t status);

 private:
  serial_impl_async_write_async_callback callback_;
  void* cookie_;
};

}  // namespace internal

class AmlUart : public ddk::SerialImplAsyncProtocol<AmlUart>,
                public fdf::WireServer<fuchsia_hardware_serialimpl::Device> {
 public:
  explicit AmlUart(ddk::PDevFidl pdev, const serial_port_info_t& serial_port_info,
                   fdf::MmioBuffer mmio, fdf::UnownedSynchronizedDispatcher irq_dispatcher)
      : pdev_(std::move(pdev)),
        serial_port_info_(serial_port_info),
        mmio_(std::move(mmio)),
        irq_dispatcher_(std::move(irq_dispatcher)) {}

  // ddk::SerialImplAsyncProtocol banjo implementation.
  zx_status_t SerialImplAsyncGetInfo(serial_port_info_t* info);
  zx_status_t SerialImplAsyncConfig(uint32_t baud_rate, uint32_t flags);
  zx_status_t SerialImplAsyncEnable(bool enable);
  void SerialImplAsyncCancelAll();
  void SerialImplAsyncReadAsync(serial_impl_async_read_async_callback callback, void* cookie);
  void SerialImplAsyncWriteAsync(const uint8_t* buf, size_t length,
                                 serial_impl_async_write_async_callback callback, void* cookie);

  // fuchsia_hardware_serialimpl::Device FIDL implementation.
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override;
  void Config(ConfigRequestView request, fdf::Arena& arena,
              ConfigCompleter::Sync& completer) override;
  void Enable(EnableRequestView request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override;
  void Read(fdf::Arena& arena, ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, fdf::Arena& arena, WriteCompleter::Sync& completer) override;
  void CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // Test functions: simulate a data race where the HandleTX / HandleRX functions get called twice.
  void HandleTXRaceForTest();
  void HandleRXRaceForTest();

  const serial_port_info_t& serial_port_info() const { return serial_port_info_; }

  void* get_ops() { return &serial_impl_async_protocol_ops_; }

 private:
  using Callback = fit::function<void(uint32_t)>;

  // Reads the current state from the status register and calls notify_cb if it has changed.
  uint32_t ReadStateAndNotify();
  uint32_t ReadState();
  void EnableLocked(bool enable) TA_REQ(enable_lock_);
  void HandleRX();
  void HandleTX();
  fit::closure MakeReadCallbackLocked(zx_status_t status, void* buf, size_t len) TA_REQ(read_lock_);
  fit::closure MakeWriteCallbackLocked(zx_status_t status) TA_REQ(write_lock_);

  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  ddk::PDevFidl pdev_;
  const serial_port_info_t serial_port_info_;
  fdf::MmioBuffer mmio_;

  bool enabled_ TA_GUARDED(enable_lock_) = false;

  Callback notify_cb_ TA_GUARDED(status_lock_) = nullptr;

  // Protects enabling/disabling lifecycle.
  fbl::Mutex enable_lock_;
  // Protects status register and notify_cb.
  fbl::Mutex status_lock_;

  // Reads
  fbl::Mutex read_lock_;
  std::optional<internal::DriverTransportReadOperation> read_operation_ TA_GUARDED(read_lock_);
  std::optional<internal::BanjoReadOperation> banjo_read_operation_ TA_GUARDED(read_lock_);

  // Writes
  fbl::Mutex write_lock_;
  std::optional<internal::DriverTransportWriteOperation> write_operation_ TA_GUARDED(write_lock_);
  std::optional<internal::BanjoWriteOperation> banjo_write_operation_ TA_GUARDED(write_lock_);
  const uint8_t* write_buffer_ TA_GUARDED(write_lock_) = nullptr;
  size_t write_size_ TA_GUARDED(write_lock_) = 0;

  fdf::UnownedSynchronizedDispatcher irq_dispatcher_;
  zx::interrupt irq_;
  async::IrqMethod<AmlUart, &AmlUart::HandleIrq> irq_handler_{this};
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_H_
