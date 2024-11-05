// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/symbolizer/log_parser.h"

#include <charconv>
#include <cstdint>
#include <string>
#include <string_view>

#include "lib/fit/defer.h"
#include "lib/syslog/cpp/macros.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/trim.h"
#include "tools/symbolizer/symbolizer.h"

namespace symbolizer {

namespace {

// This need to match
// https://github.com/dart-lang/sdk/blob/f424f3a4cca306513e77c7747682f1c1c99e3307/runtime/vm/object.cc#L25501
constexpr std::string_view kDartStackTraceMagic =
    "*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***";

// Converts the string in dec or hex into an integer. Returns whether the conversion is complete.
template <typename int_t>
bool ParseInt(std::string_view string, int_t &i, int base = 10) {
  if (string.empty())
    return false;

  const char *begin = string.data();
  const char *end = begin + string.size();
  if (string.size() > 2 && string[0] == '0' && string[1] == 'x') {
    base = 16;
    begin += 2;
  }
  return std::from_chars(begin, end, i, base).ptr == end;
}

}  // namespace

bool LogParser::ProcessNextLine() {
  std::string line;

  std::getline(input_, line);
  if (input_.eof() && line.empty()) {
    return false;
  }

  // Handle symbolizer markup.
  auto start = line.find("{{{");
  auto end = std::string::npos;

  if (start != std::string::npos) {
    end = line.find("}}}", start);
  }
  if (end != std::string::npos) {
    std::string_view line_view(line);
    auto output = CreateOutputFn(line_view.substr(0, start), line_view.substr(end + 3));
    if (ProcessMarkup(line_view.substr(start + 3, end - start - 3), std::move(output))) {
      // Skip outputting only if we have the starting and the ending braces and the markup is valid.
      return true;
    }
  }

  // Handle Dart symbolization.
  if (line == kDartStackTraceMagic) {
    symbolizing_dart_ = true;
  } else if (symbolizing_dart_) {
    if (ProcessDart(line, CreateOutputFn("", ""))) {
      return true;
    }
    symbolizing_dart_ = false;
  }

  OutputRaw(line);
  return true;
}

bool LogParser::ProcessMarkup(std::string_view markup, Symbolizer::StringOutputFn output) {
  auto splitted = fxl::SplitString(markup, ":", fxl::kKeepWhitespace, fxl::kSplitWantAll);
  if (splitted.empty()) {
    return false;
  }

  auto tag = splitted[0];

  if (tag == "reset") {
    auto type = Symbolizer::ResetType::kUnknown;
    if (splitted.size() >= 2) {
      if (splitted[1] == "begin") {
        type = Symbolizer::ResetType::kBegin;
      } else if (splitted[1] == "end") {
        type = Symbolizer::ResetType::kEnd;
      }
    }
    symbolizer_->Reset(false, type);
    return true;
  }

  if (tag == "module") {
    // module:0x{id}:{name}:elf:{build_id}
    if (splitted.size() < 5)
      return false;

    uint64_t id;
    if (!ParseInt(splitted[1], id) || splitted[3] != "elf")
      return false;

    symbolizer_->Module(id, splitted[2], splitted[4]);
    return true;
  }

  if (tag == "mmap") {
    // mmap:0x{address}:0x{size}:load:0x{module_id}:r?w?x?:0x{module_offset}
    if (splitted.size() < 7)
      return false;

    uint64_t address;
    uint64_t size;
    uint64_t module_id;
    uint64_t module_offset;

    if (!ParseInt(splitted[1], address) || !ParseInt(splitted[2], size) ||
        !ParseInt(splitted[4], module_id) || !ParseInt(splitted[6], module_offset) ||
        splitted[3] != "load")
      return false;

    symbolizer_->MMap(address, size, module_id, splitted[5], module_offset, std::move(output));
    return true;
  }

  if (tag == "bt") {
    // bt:{frame_id}:{address}(:ra|:pc)?(:msg)?
    if (splitted.size() < 3)
      return false;

    int frame_id;
    uint64_t address;
    Symbolizer::AddressType type = Symbolizer::AddressType::kUnknown;
    std::string_view message;

    if (!ParseInt(splitted[1], frame_id) || !ParseInt(splitted[2], address))
      return false;

    // Optional suffix(es).
    if (splitted.size() >= 4) {
      if (splitted[3] == "ra") {
        type = Symbolizer::AddressType::kReturnAddress;
      } else if (splitted[3] == "pc") {
        type = Symbolizer::AddressType::kProgramCounter;
      } else {
        message = splitted[3];
      }
      if (splitted.size() >= 5) {
        message = splitted[4];
      }
    }
    symbolizer_->Backtrace(frame_id, address, type, message, std::move(output));
    return true;
  }

  if (tag == "dumpfile") {
    // dumpfile:{type}:{name}
    if (splitted.size() < 3)
      return false;

    symbolizer_->DumpFile(splitted[1], splitted[2]);
    return true;
  }

  return false;
}

// If returning true, we're responsible to output the line.
bool LogParser::ProcessDart(std::string_view line, Symbolizer::StringOutputFn output) {
  constexpr uint64_t kModuleId = 0;
  constexpr uint64_t kModuleSize = 0x800000000;  // 32 GB should be big enough.

  auto splitted = fxl::SplitString(line, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);

  if (splitted.size() == 6 && splitted[0] == "pid:") {
    // pid: 12, tid: 30221, name some.ui
    dart_process_name_ = splitted[5];
    symbolizer_->Reset(true, Symbolizer::ResetType::kUnknown);
  } else if (splitted.size() == 2 && splitted[0] == "build_id:") {
    // build_id: '0123456789abcdef'
    symbolizer_->Module(kModuleId, dart_process_name_, fxl::TrimString(splitted[1], "'"));
  } else if (splitted.size() == 4 && splitted[0] == "isolate_dso_base:") {
    // isolate_dso_base: f2e4c8000, vm_dso_base: f2e4c8000
    uint64_t address;
    if (!ParseInt(splitted[3], address, 16)) {
      return false;
    }
    symbolizer_->MMap(address, kModuleSize, kModuleId, "", 0, std::move(output));
  } else if (!splitted.empty() &&
             (splitted[0] == "os:" || splitted[0] == "isolate_instructions:")) {
    // os: fuchsia arch: arm64 comp: no sim: no
    // isolate_instructions: f2f9f8e60, vm_instructions: f2f9f4000
  } else if (splitted.size() >= 6 && splitted[0][0] == '#' && splitted[1] == "abs") {
    // #00 abs 0000000f2fbb51c7 virt 00000000016ed1c7 _kDartIsolateSnapshotInstructions+0x1bc367
    uint64_t frame_id;
    uint64_t address;
    if (!ParseInt(splitted[0].substr(1), frame_id)) {
      return false;
    }
    if (!ParseInt(splitted[2], address, 16)) {
      return false;
    }
    symbolizer_->Backtrace(frame_id, address, Symbolizer::AddressType::kUnknown, "",
                           std::move(output));
    return true;
  } else {
    return false;
  }

  // Don't forget to output the context as is.
  OutputRaw(line);
  return true;
}

Symbolizer::StringOutputFn LogParser::CreateOutputFn(std::string_view prefix,
                                                     std::string_view suffix) {
  // Our design requires that the output must be in the same order of the input, which means the
  // the destruction of OutputFn must be in the same order of the construction.
  output_buffers_.emplace_back();
  // Use a pointer to the buffer as a guard to ensure such order, because std::deque won't
  // invalidate reference on resizing.
  std::string *buffer = &output_buffers_.back();
  auto on_drop = fit::defer([this, buffer]() {
    FX_CHECK(buffer == &output_buffers_.front());
    output_ << output_buffers_.front();
    output_buffers_.pop_front();
  });
  return [this, prefix = std::string(prefix), suffix = std::string(suffix),
          on_drop = std::move(on_drop)](std::string_view content) {
    output_ << prefix << content << suffix << '\n';
  };
}

void LogParser::OutputRaw(std::string_view message) {
  if (!output_buffers_.empty()) {
    output_buffers_.back() += message;
    output_buffers_.back() += '\n';
  } else {
    output_ << message << '\n';
  }
}

}  // namespace symbolizer
