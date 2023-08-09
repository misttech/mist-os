// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "samples_to_pprof.h"

#include <lib/fit/result.h>

#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "src/performance/profiler/profile/profile.pb.h"

Interner::Interner(perfetto::third_party::perftools::profiles::Profile* profile)
    : string_table_(profile->mutable_string_table()),
      function_table_(profile->mutable_function()),
      location_table_(profile->mutable_location()),
      sample_table_(profile->mutable_sample()) {
  // string_table[0] is always "".
  string_table_->Add("");
}

void Interner::AddSample(const std::vector<BackTraceEntry>& entries) {
  perfetto::third_party::perftools::profiles::Sample* sample = sample_table_->Add();
  for (const BackTraceEntry& entry : entries) {
    uint64_t location_id = InternLocation(entry);
    sample->add_location_id(location_id);
  }
  sample->add_value(1);
}

uint64_t Interner::InternLocation(const BackTraceEntry& entry) {
  perfetto::third_party::perftools::profiles::Location* location = location_table_->Add();
  location->set_id(location_counter_++);
  location->set_address(entry.addr);
  perfetto::third_party::perftools::profiles::Line* line = location->add_line();
  line->set_function_id(InternFunction(entry.function_name, entry.file_name, entry.line_no));
  line->set_line(entry.line_no);

  return location->id();
}

int64_t Interner::InternString(std::string value) {
  auto itr = value_to_index_.find(value);
  if (itr != value_to_index_.end()) {
    return itr->second;
  }
  int64_t index = value_to_index_[value] = string_table_->size();
  string_table_->Add(std::move(value));
  return index;
}

uint64_t Interner::InternFunction(std::string function_name, std::string file_name,
                                  int64_t line_no) {
  FunctionEntry entry{std::move(function_name), std::move(file_name), line_no};
  auto itr = function_to_index_.find(entry);
  if (itr != function_to_index_.end()) {
    return itr->second;
  }

  uint64_t id = function_to_index_.size() + 1;
  perfetto::third_party::perftools::profiles::Function* function = function_table_->Add();
  function->set_id(id);
  function->set_name(InternString(entry.function_name));
  function->set_system_name(InternString(entry.function_name));
  function->set_filename(InternString(entry.file_name));
  function->set_start_line(line_no);
  function_to_index_[entry] = function->id();
  return function->id();
}

bool Interner::FunctionEntry::operator<(const FunctionEntry& other) const {
  return function_name != other.function_name ? function_name < other.function_name
         : file_name != other.file_name       ? file_name < other.file_name
                                              : line_no < other.line_no;
}

std::optional<BackTraceEntry> parseBackTraceEntry(std::string s) {
  uint64_t addr;
  std::string function_name;
  std::string file_name;
  int64_t line_no = 0;

  // An entry looks like:
  //   #0    0x000002b61427d017 in add(uint64_t*)
  //   ../../src/performance/profiler/test/demo_target/main.cc:10 <<VMO#36955=blob-89cc36de>>+0x1017
  //   sp 0x33da01c0f30
  size_t counter_pos = s.find('#');
  size_t addr_begin = counter_pos + 6;
  size_t addr_end = s.find(' ', addr_begin);
  addr = strtoll(s.data() + addr_begin, nullptr, 0);
  if (addr == 0) {
    return std::nullopt;
  }
  size_t func_name_begin = s.find(' ', addr_end + 1);
  if (func_name_begin == std::string::npos) {
    return {{addr, function_name, file_name, line_no}};
  }

  // We found the index of the <space> before the function name
  func_name_begin += 1;

  // Function names can be very complex and contain nearly any character, so there isn't a
  // consistent way to parse them short of implementing a full on parser. Instead, we work backwards
  // to parse out the module and file name and if we have anything left when we meet in the middle,
  // that's the file name.
  size_t module_begin = s.rfind(' ');

  if (module_begin == func_name_begin) {
    // We don't have line info, instead we just have something like
    //    #0    0x000040934d4f0d98 in <libzircon.so>+0x8d98
    // We'll just call that the function name
    std::string module_name = s.substr(module_begin, s.size() - module_begin);
    return {{addr, module_name, module_name, 0}};
  }

  size_t file_name_begin = s.rfind(' ', module_begin - 1);
  // We found the index of the <space> before the file name
  file_name_begin += 1;
  size_t file_name_end = s.rfind(':', module_begin - 1);

  size_t func_name_end = file_name_begin - 1;

  // We may not have a function name if we don't have symbolization info for the specific object.
  // When this happens, our pointers from the front and back will cross over.
  if (func_name_end > func_name_begin) {
    function_name = s.substr(func_name_begin, func_name_end - func_name_begin);
  }
  file_name = s.substr(file_name_begin, file_name_end - file_name_begin);

  size_t line_no_begin = file_name_end + 1;
  if (line_no_begin == std::string::npos) {
    return {{addr, function_name, file_name, line_no}};
  }
  line_no = strtoll(s.data() + line_no_begin, nullptr, 0);

  return {{addr, function_name, file_name, line_no}};
}

fit::result<std::string, perfetto::third_party::perftools::profiles::Profile> samples_to_profile(
    std::ifstream in) {
  perfetto::third_party::perftools::profiles::Profile pprof;
  Interner interner(&pprof);

  auto* value_type = pprof.add_sample_type();
  value_type->set_type(interner.InternString("location"));
  value_type->set_unit(interner.InternString("count"));

  std::vector<BackTraceEntry> entries;
  std::string pid;
  std::string tid;
  enum ParsingState {
    MODULES,
    PID_TID,
    FIRST_ENTRY,
    ENTRIES,
  };
  ParsingState state = MODULES;
  // The sample file looks like:
  //
  // [[[ELF module declaration]]
  // [[[ELF module declaration]]
  // ...
  // [[[ELF module declaration]]
  // <pid>
  // <tid>
  //    #0    <addr> in <function> <file>:<line> <library>+<offset>
  //    #1    <addr> in <function> <file>:<line> <library>+<offset>
  //    ...
  //    #<n>    <addr> in <function> <file>:<line> <library>+<offset>
  // <pid>
  // <tid>
  //    #0    <addr> in <function> <file>:<line> <library>+<offset>
  //    #1    <addr> in <function> <file>:<line> <library>+<offset>
  //    ...
  //    #<n>    <addr> in <function> <file>:<line> <library>+<offset>
  //
  // The following loop parses it using a state machine that looks like:
  //
  //  [ START ]
  //     |
  //     v
  //  [ MODULES ] -> [ PID_TID ] -> [ FIRST_ENTRY ]
  //    ^   |             ^              |
  //    \---/             |              v
  //                      \-------- [ ENTRIES ]-\
  //                                      ^     |
  //                                      \-----/
  //
  for (std::string line; std::getline(in, line);) {
    switch (state) {
      case MODULES:
        // Skip the ELF module declarations
        if (!std::isdigit(line[0])) {
          break;
        } else {
          state = PID_TID;
          pid = line;
          break;
        }
      case PID_TID: {
        state = FIRST_ENTRY;
        tid = line;
        break;
      }
      case FIRST_ENTRY: {
        if (!entries.empty()) {
          interner.AddSample(entries);
          entries.clear();
        }
        std::optional<BackTraceEntry> entry = parseBackTraceEntry(line);
        if (entry && entry->addr != 0) {
          entries.push_back(*entry);
        }
        state = ENTRIES;
        break;
      }
      case ENTRIES: {
        if (line[0] != ' ') {
          // Include two artificial frames containing the pid and tid of the sample. This way
          // profiles which include samples from multiple threads and processes will have their
          // samples properly nested.
          entries.emplace_back(0xDEADBEEF, "tid: " + tid, "<>", 1);
          entries.emplace_back(0xDEADBEEF, "pid: " + pid, "<>", 1);
          pid = line;
          state = PID_TID;
          break;
        }
        std::optional<BackTraceEntry> entry = parseBackTraceEntry(line);
        if (entry) {
          entries.push_back(*entry);
        }
        break;
      }
    }
  }
  if (!entries.empty()) {
    interner.AddSample(entries);
  }
  return fit::ok(pprof);
}
