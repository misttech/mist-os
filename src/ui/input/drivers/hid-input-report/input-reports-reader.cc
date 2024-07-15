// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "input-reports-reader.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/trace/event.h>

namespace hid_input_report_dev {

namespace {

constexpr size_t kPerLine = 16;

void hexdump(const cpp20::span<const uint8_t> data) {
  constexpr uint32_t kCharsPerByte = 3;
  for (size_t i = 0; i < data.size_bytes(); i += kPerLine) {
    size_t line_size = std::min(kPerLine, data.size_bytes() - i);
    char line[kCharsPerByte * line_size];
    for (size_t j = 0; j < line_size; j++) {
      sprintf(&line[kCharsPerByte * j], "%02X ", data[i + j]);
    }

    FDF_LOG(INFO, "hid-dump(%zu): %s", i, line);
  }
}

}  // namespace

void InputReportsReader::ReadInputReports(ReadInputReportsCompleter::Sync& completer) {
  if (waiting_read_) {
    completer.ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  waiting_read_ = completer.ToAsync();
  SendReportsToWaitingRead();
}

void InputReportsReader::SendReportsToWaitingRead() {
  if (reports_data_.empty()) {
    return;
  }

  std::array<fuchsia_input_report::wire::InputReport,
             fuchsia_input_report::wire::kMaxDeviceReportCount>
      reports;
  size_t num_reports = 0;

  TRACE_DURATION("input", "InputReportInstance GetReports", "instance_id", reader_id_);
  while (!reports_data_.empty()) {
    TRACE_FLOW_STEP("input", "input_report", reports_data_.front().trace_id());
    reports[num_reports++] = std::move(reports_data_.front());
    reports_data_.pop();
  }

  waiting_read_->ReplySuccess(
      fidl::VectorView<fuchsia_input_report::wire::InputReport>::FromExternal(reports.data(),
                                                                              num_reports));
  fidl::Status result = waiting_read_->result_of_reply();
  if (!result.ok()) {
    FDF_LOG(ERROR, "SendReport: Failed to send reports: %s\n", result.FormatDescription().c_str());
  }
  waiting_read_.reset();

  // We have sent the reports so reset the allocator.
  report_allocator_.Reset();
}

void InputReportsReader::ReceiveReport(cpp20::span<const uint8_t> raw_report, zx::time report_time,
                                       hid_input_report::Device* device) {
  fuchsia_input_report::wire::InputReport report(report_allocator_);

  if (!device->InputReportId().has_value()) {
    FDF_LOG(ERROR, "ReceiveReport: Device cannot receive input reports\n");
    return;
  }

  if (hid_input_report::ParseResult result =
          device->ParseInputReport(raw_report.data(), raw_report.size(), report_allocator_, report);
      result != hid_input_report::ParseResult::kOk) {
    FDF_LOG(ERROR, "ReceiveReport: Device failed to parse report correctly %s (%d)",
            ParseResultGetString(result), static_cast<int>(result));
    hexdump(raw_report);
    return;
  }

  report.set_report_id(*device->InputReportId());
  report.set_event_time(report_allocator_, report_time.get());
  report.set_trace_id(report_allocator_, TRACE_NONCE());

  // If we are full, pop the oldest report.
  if (reports_data_.full()) {
    reports_data_.pop();
  }

  reports_data_.push(std::move(report));
  TRACE_FLOW_BEGIN("input", "input_report", reports_data_.back().trace_id());

  if (waiting_read_) {
    SendReportsToWaitingRead();
  }
}

}  // namespace hid_input_report_dev
