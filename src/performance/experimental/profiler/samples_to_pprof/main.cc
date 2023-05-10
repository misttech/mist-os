// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/result.h>

#include <fstream>
#include <iostream>

#include "src/performance/profiler/profile/profile.pb.h"

struct BackTraceEntry {
  uint64_t addr;
  std::string function_name;
  std::string file_name;
  int64_t line_no;
};

class Interner {
 public:
  explicit Interner(perfetto::third_party::perftools::profiles::Profile* profile)
      : string_table_(profile->mutable_string_table()),
        function_table_(profile->mutable_function()),
        location_table_(profile->mutable_location()),
        sample_table_(profile->mutable_sample()) {
    // string_table[0] is always "".
    string_table_->Add("");
  }

  void AddSample(const std::vector<BackTraceEntry>& entries) {
    perfetto::third_party::perftools::profiles::Sample* sample = sample_table_->Add();
    for (const BackTraceEntry& entry : entries) {
      uint64_t location_id = InternLocation(entry);
      sample->add_location_id(location_id);
    }
    sample->add_value(1);
  }

  uint64_t InternLocation(const BackTraceEntry& entry) {
    perfetto::third_party::perftools::profiles::Location* location = location_table_->Add();
    location->set_id(location_counter_++);
    location->set_address(entry.addr);
    perfetto::third_party::perftools::profiles::Line* line = location->add_line();
    line->set_function_id(InternFunction(entry.function_name, entry.file_name, entry.line_no));
    line->set_line(entry.line_no);

    return location->id();
  }

  int64_t InternString(std::string value) {
    auto itr = value_to_index_.find(value);
    if (itr != value_to_index_.end()) {
      return itr->second;
    }
    int64_t index = value_to_index_[value] = string_table_->size();
    string_table_->Add(std::move(value));
    return index;
  }

  uint64_t InternFunction(std::string function_name, std::string file_name, int64_t line_no) {
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

 private:
  uint64_t location_counter_ = 1;
  std::map<std::string, int64_t> value_to_index_;
  struct FunctionEntry {
    std::string function_name;
    std::string file_name;
    int64_t line_no;

    bool operator<(const FunctionEntry& other) const {
      return function_name != other.function_name ? function_name < other.function_name
             : file_name != other.file_name       ? file_name < other.file_name
                                                  : line_no < other.line_no;
    }
  };
  std::map<FunctionEntry, uint64_t> function_to_index_;
  google::protobuf::RepeatedPtrField<std::string>* string_table_;
  google::protobuf::RepeatedPtrField<perfetto::third_party::perftools::profiles::Function>*
      function_table_;
  google::protobuf::RepeatedPtrField<perfetto::third_party::perftools::profiles::Location>*
      location_table_;
  google::protobuf::RepeatedPtrField<perfetto::third_party::perftools::profiles::Sample>*
      sample_table_;
};

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

  size_t func_name_end = s.find("../", func_name_begin);
  if (func_name_end == std::string::npos) {
    return {{addr, function_name, file_name, line_no}};
  }

  function_name = s.substr(func_name_begin, func_name_end - func_name_begin);
  size_t file_name_begin = func_name_end;
  if (file_name_begin == std::string::npos) {
    return {{addr, function_name, file_name, line_no}};
  }
  size_t file_name_end = s.find(':', file_name_begin);
  if (file_name_end == std::string::npos) {
    return {{addr, function_name, file_name, line_no}};
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

  bool reading_bt = false;
  std::vector<BackTraceEntry> entries;
  for (std::string line; std::getline(in, line);) {
    // Skip until we find a backtrace starting point
    if (!reading_bt) {
      if (line[0] != ' ') {
        continue;
      }
      reading_bt = true;
    }

    if (line[4] == '0') {
      if (!entries.empty()) {
        interner.AddSample(entries);
        entries.clear();
      }
    }
    std::optional<BackTraceEntry> entry = parseBackTraceEntry(line);
    if (entry) {
      entries.push_back(*entry);
    }
  }
  if (!entries.empty()) {
    interner.AddSample(entries);
  }
  return fit::ok(pprof);
}

int main(int argc, char* argv[]) {
  if (argc < 2) {
    std::cerr << "usage: " << argv[0] << " SAMPLES.out" << std::endl;
    return 1;
  }
  std::string out_path(argv[1]);
  out_path += ".pb";

  std::ifstream ifs;
  ifs.open(argv[1]);

  auto pprof = samples_to_profile(std::move(ifs));
  if (pprof.is_error()) {
    std::cout << "Writing samples failed: " << std::endl;
    std::cout << pprof.error_value() << std::endl;
    return 1;
  }
  std::ofstream output(out_path.c_str(), std::ios::out);
  pprof.value().SerializePartialToOstream(&output);
}
