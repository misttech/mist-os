// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIGHT_SENSOR_DRIVERS_AMS_LIGHT_TCS3400_H_
#define SRC_DEVICES_LIGHT_SENSOR_DRIVERS_AMS_LIGHT_TCS3400_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async/cpp/irq.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/device-protocol/i2c-channel.h>
#include <lib/input_report_reader/reader.h>
#include <lib/inspect/contrib/cpp/bounded_list_node.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <time.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/mutex.h>

namespace tcs {

struct Tcs3400InputReport {
  zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
  int64_t illuminance;
  int64_t red;
  int64_t blue;
  int64_t green;

  void ToFidlInputReport(
      fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
      fidl::AnyArena& allocator) const;

  bool is_valid() const { return event_time.get() != ZX_TIME_INFINITE_PAST; }
};

struct Tcs3400FeatureReport {
  zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
  int64_t report_interval_us = 0;
  fuchsia_input_report::wire::SensorReportingState reporting_state;
  int64_t sensitivity = 0;
  int64_t threshold_high = 0;
  int64_t threshold_low = 0;
  int64_t integration_time_us = 0;

  fuchsia_input_report::wire::FeatureReport ToFidlFeatureReport(fidl::AnyArena& allocator) const;
};

struct InspectTcs3400FeatureReport {
  inspect::Node node;
  inspect::UintProperty event_time;
  inspect::UintProperty report_interval_us;
  inspect::StringProperty reporting_state;
  inspect::UintProperty sensitivity;
  inspect::UintProperty threshold_high;
  inspect::UintProperty threshold_low;
  inspect::UintProperty integration_time_us;

  explicit InspectTcs3400FeatureReport(inspect::Node n, const Tcs3400FeatureReport& report);
};

class Tcs3400Device;
using DeviceType =
    ddk::Device<Tcs3400Device, ddk::Messageable<fuchsia_input_report::InputDevice>::Mixin,
                ddk::Unbindable>;

class Tcs3400Device : public DeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_INPUTREPORT> {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  Tcs3400Device(zx_device_t* device, async_dispatcher_t* dispatcher, ddk::I2cChannel i2c,
                fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> gpio);
  ~Tcs3400Device() override = default;

  zx_status_t Bind();
  zx_status_t InitMetadata();

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override;

  void GetDescriptor(GetDescriptorCompleter::Sync& completer) override;
  void SendOutputReport(SendOutputReportRequestView request,
                        SendOutputReportCompleter::Sync& completer) override;
  void GetFeatureReport(GetFeatureReportCompleter::Sync& completer) override;
  void SetFeatureReport(SetFeatureReportRequestView request,
                        SetFeatureReportCompleter::Sync& completer) override;
  void GetInputReport(GetInputReportRequestView request,
                      GetInputReportCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_input_report::InputDevice> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    zxlogf(WARNING, "Unexpected fidl method invoked: %ld", metadata.method_ordinal);
  }

  // Visible for testing.
  void WaitForNextReader();
  inspect::Inspector& inspect() { return inspect_; }

 private:
  static constexpr size_t kFeatureAndDescriptorBufferSize = 512;

  void HandlePoll();
  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);
  void RearmIrq();
  void Configure();

  async_dispatcher_t* dispatcher_;
  async::IrqMethod<Tcs3400Device, &Tcs3400Device::HandleIrq> irq_handler_{this};
  async::TaskClosureMethod<Tcs3400Device, &Tcs3400Device::HandlePoll> polling_handler_{this};
  async::TaskClosureMethod<Tcs3400Device, &Tcs3400Device::RearmIrq> rearm_irq_handler_{this};

  ddk::I2cChannel i2c_;  // Accessed by the main thread only before thread_ has been started.
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio_;
  zx::interrupt irq_;
  fbl::Mutex input_lock_;
  fbl::Mutex feature_lock_;
  Tcs3400InputReport input_rpt_ TA_GUARDED(input_lock_) = {};
  Tcs3400FeatureReport feature_rpt_ TA_GUARDED(feature_lock_) = {};
  uint8_t atime_ = 1;
  uint8_t again_ = 1;
  bool isSaturated_ = false;
  zx::time lastSaturatedLog_ = zx::time::infinite_past();
  sync_completion_t next_reader_wait_;
  input_report_reader::InputReportReaderManager<Tcs3400InputReport,
                                                fuchsia_input_report::wire::kMaxDeviceReportCount>
      readers_;
  inspect::Inspector inspect_;
  inspect::contrib::BoundedListNode inspect_reports_;

  zx::result<Tcs3400InputReport> ReadInputRpt();
  zx_status_t InitGain(uint8_t gain);
  zx_status_t WriteReg(uint8_t reg, uint8_t value);
  zx_status_t ReadReg(uint8_t reg, uint8_t& output_value);
};
}  // namespace tcs

#endif  // SRC_DEVICES_LIGHT_SENSOR_DRIVERS_AMS_LIGHT_TCS3400_H_
