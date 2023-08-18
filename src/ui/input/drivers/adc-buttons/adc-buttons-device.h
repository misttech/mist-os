// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_ADC_BUTTONS_ADC_BUTTONS_DEVICE_H_
#define SRC_UI_INPUT_DRIVERS_ADC_BUTTONS_ADC_BUTTONS_DEVICE_H_

#include <fidl/fuchsia.buttons/cpp/fidl.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/input_report_reader/reader.h>

#include <set>

#include <fbl/ref_ptr.h>
#include <soc/aml-common/aml-g12-saradc.h>

namespace adc_buttons_device {

class AdcButtonsDevice : public fidl::WireServer<fuchsia_input_report::InputDevice> {
 public:
  AdcButtonsDevice(async_dispatcher_t* dispatcher, std::unique_ptr<AmlSaradcDevice> saradc,
                   uint32_t polling_rate_usec,
                   std::map<uint32_t, std::vector<fuchsia_buttons::Button>> configs,
                   std::set<fuchsia_input_report::ConsumerControlButton> buttons)
      : dispatcher_(dispatcher),
        polling_rate_usec_(polling_rate_usec),
        saradc_(std::move(saradc)),
        configs_(std::move(configs)),
        buttons_(std::move(buttons)) {
    saradc_->HwInit();
    polling_task_.Post(dispatcher_);
  }
  void Shutdown();

  // fuchsia_input_report::InputDevice required methods
  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override;
  void GetDescriptor(GetDescriptorCompleter::Sync& completer) override;
  void SendOutputReport(SendOutputReportRequestView request,
                        SendOutputReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetFeatureReport(GetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetFeatureReport(SetFeatureReportRequestView request,
                        SetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetInputReport(GetInputReportRequestView request,
                      GetInputReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  friend class AdcButtonsDeviceTest;

  void PollingTask(async_dispatcher_t* dispatcher, async::TaskBase* task, zx_status_t status);

  async_dispatcher_t* const dispatcher_;

  async::TaskMethod<AdcButtonsDevice, &AdcButtonsDevice::PollingTask> polling_task_{this};
  uint32_t polling_rate_usec_;

  std::unique_ptr<AmlSaradcDevice> saradc_;

  // Maps channels to a list of button configs.
  std::map<uint32_t, std::vector<fuchsia_buttons::Button>> configs_;
  std::set<fuchsia_input_report::ConsumerControlButton> buttons_;  // For descriptor

  static constexpr size_t kFeatureAndDescriptorBufferSize = 512;
  struct AdcButtonInputReport {
    zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
    std::set<fuchsia_input_report::ConsumerControlButton> buttons;

    void ToFidlInputReport(
        fidl::WireTableBuilder<fuchsia_input_report::wire::InputReport>& input_report,
        fidl::AnyArena& allocator);
  };
  AdcButtonInputReport rpt_;
  input_report_reader::InputReportReaderManager<AdcButtonInputReport> readers_;
};

}  // namespace adc_buttons_device

#endif  // SRC_UI_INPUT_DRIVERS_ADC_BUTTONS_ADC_BUTTONS_DEVICE_H_
