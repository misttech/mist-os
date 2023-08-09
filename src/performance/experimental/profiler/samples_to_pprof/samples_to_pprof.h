// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLES_TO_PPROF_SAMPLES_TO_PPROF_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLES_TO_PPROF_SAMPLES_TO_PPROF_H_

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

struct BackTraceEntry {
  BackTraceEntry(uint64_t addr, std::string function_name, std::string file_name, int64_t line_no)
      : addr(addr),
        function_name(std::move(function_name)),
        file_name(std::move(file_name)),
        line_no(line_no) {}
  uint64_t addr;
  std::string function_name;
  std::string file_name;
  int64_t line_no;
};

class Interner {
 public:
  explicit Interner(perfetto::third_party::perftools::profiles::Profile* profile);

  void AddSample(const std::vector<BackTraceEntry>& entries);

  uint64_t InternLocation(const BackTraceEntry& entry);
  int64_t InternString(std::string value);
  uint64_t InternFunction(std::string function_name, std::string file_name, int64_t line_no);

 private:
  uint64_t location_counter_ = 1;
  std::map<std::string, int64_t> value_to_index_;
  struct FunctionEntry {
    std::string function_name;
    std::string file_name;
    int64_t line_no;

    bool operator<(const FunctionEntry& other) const;
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

// Given symbolizer formatted string that look like:
//
//   #0    0x000002b61427d017 in add(uint64_t*)
//   ../../src/performance/profiler/test/demo_target/main.cc:10 <<VMO#36955=blob-89cc36de>>+0x1017
//
//   Parse them into a BackTraceEntry.
std::optional<BackTraceEntry> parseBackTraceEntry(std::string s);

// Given a file containing formatted symbolizer output, consolidate it into the data structure used
// by pprof.
fit::result<std::string, perfetto::third_party::perftools::profiles::Profile> samples_to_profile(
    std::ifstream in);

#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLES_TO_PPROF_SAMPLES_TO_PPROF_H_
