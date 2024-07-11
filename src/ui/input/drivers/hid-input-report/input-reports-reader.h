// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_HID_INPUT_REPORT_INPUT_REPORTS_READER_H_
#define SRC_UI_INPUT_DRIVERS_HID_INPUT_REPORT_INPUT_REPORTS_READER_H_

#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <fbl/ring_buffer.h>

#include "src/ui/input/lib/hid-input-report/device.h"

namespace hid_input_report_dev {

class InputReportsReader;

class InputReportBase {
 public:
  virtual void RemoveReaderFromList(InputReportsReader* reader) = 0;
};

class InputReportsReader : public fidl::WireServer<fuchsia_input_report::InputReportsReader> {
 public:
  // The InputReportBase has to exist for the lifetime of the InputReportsReader.
  // The pointer to InputReportBase is unowned.
  // InputReportsReader will be freed by InputReportBase.
  explicit InputReportsReader(InputReportBase* base, uint32_t reader_id,
                              fidl::ServerEnd<fuchsia_input_report::InputReportsReader> server)
      : reader_id_(reader_id),
        binding_(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this,
                 [this](fidl::UnbindInfo) { base_->RemoveReaderFromList(this); }),
        base_(base) {}
  ~InputReportsReader() override { binding_.Close(ZX_ERR_PEER_CLOSED); }

  void ReceiveReport(cpp20::span<const uint8_t> raw_report, zx::time report_time,
                     hid_input_report::Device* device);

  // FIDL functions.
  void ReadInputReports(ReadInputReportsCompleter::Sync& completer) override;

 private:
  // This is the static size that is used to allocate this instance's InputReports that
  // are stored in `reports_data`. This amount of memory is allocated with the driver
  // when the driver is initialized. If the `InputReports` go over this limit the
  // rest of the memory will be heap allocated.
  static constexpr size_t kFidlReportBufferSize = 8192;

  void SendReportsToWaitingRead();

  const uint32_t reader_id_;
  std::optional<InputReportsReader::ReadInputReportsCompleter::Async> waiting_read_;
  fidl::ServerBinding<fuchsia_input_report::InputReportsReader> binding_;
  fidl::Arena<kFidlReportBufferSize> report_allocator_;
  fbl::RingBuffer<fuchsia_input_report::wire::InputReport,
                  fuchsia_input_report::wire::kMaxDeviceReportCount>
      reports_data_;

  InputReportBase* base_;
};

}  // namespace hid_input_report_dev

#endif  // SRC_UI_INPUT_DRIVERS_HID_INPUT_REPORT_INPUT_REPORTS_READER_H_
