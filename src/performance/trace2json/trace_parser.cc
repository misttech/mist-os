// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace2json/trace_parser.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/assert.h>

#include <utility>

#include "src/performance/lib/trace_converters/chromium_exporter.h"

namespace tracing {

FuchsiaTraceParser::FuchsiaTraceParser(const std::filesystem::path& out)
    : exporter_(out),
      reader_([this](trace::Record record) { exporter_.ExportRecord(record); },
              [](std::string_view error) { FX_LOGS(ERROR) << error; }) {}

FuchsiaTraceParser::~FuchsiaTraceParser() = default;

bool FuchsiaTraceParser::ParseComplete(std::istream* in) {
  // First pass: Read all records except scheduler events.
  ZX_ASSERT(exporter_.pass_ == ChromiumExporter::Pass::kMain);
  while (true) {
    size_t bytes_read =
        in->read(buffer_.data() + buffer_end_, buffer_.size() - buffer_end_).gcount();
    if (bytes_read == 0) {
      // End of file reached.
      break;
    }
    buffer_end_ += bytes_read;

    size_t words = buffer_end_ / sizeof(uint64_t);
    const uint64_t* data_ptr = reinterpret_cast<const uint64_t*>(buffer_.data());

    trace::Chunk main_chunk(data_ptr, words);
    if (!reader_.ReadRecords(main_chunk)) {
      FX_LOGS(ERROR) << "Error parsing trace";
      return false;
    }

    size_t offset = main_chunk.current_byte_offset();
    memmove(buffer_.data(), buffer_.data() + offset, buffer_end_ - offset);
    buffer_end_ -= offset;
  }

  if (buffer_end_ > 0) {
    FX_LOGS(ERROR) << "Trace file did not end at a record boundary.";
    return false;
  }

  // Second pass: read only scheduler events.
  // The second pass is for scheduler events. These events need to be
  // processed after all other events so that we have a complete view of
  // all the threads that existed during the trace.
  exporter_.StartSchedulerPass();
  in->clear();
  in->seekg(0, std::ios::beg);
  buffer_end_ = 0;

  while (true) {
    size_t bytes_read =
        in->read(buffer_.data() + buffer_end_, buffer_.size() - buffer_end_).gcount();
    if (bytes_read == 0) {
      // End of file reached.
      break;
    }
    buffer_end_ += bytes_read;

    size_t words = buffer_end_ / sizeof(uint64_t);
    const uint64_t* data_ptr = reinterpret_cast<const uint64_t*>(buffer_.data());

    trace::Chunk scheduler_chunk(data_ptr, words);
    if (!reader_.ReadRecords(scheduler_chunk)) {
      FX_LOGS(ERROR) << "Error parsing scheduler event in trace";
      return false;
    }

    size_t offset = scheduler_chunk.current_byte_offset();
    memmove(buffer_.data(), buffer_.data() + offset, buffer_end_ - offset);
    buffer_end_ -= offset;
  }

  if (buffer_end_ > 0) {
    FX_LOGS(ERROR) << "Trace file did not end at a record boundary when parsing scheduler event.";
    return false;
  }

  return true;
}

}  // namespace tracing
