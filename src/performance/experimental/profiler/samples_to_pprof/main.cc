// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/result.h>

#include <fstream>
#include <iostream>
#include <string>
#include <utility>

#include "samples_to_pprof.h"
#include "src/performance/profiler/profile/profile.pb.h"

int main(int argc, char* argv[]) {
  if (argc < 2) {
    std::cerr << "usage: " << argv[0] << " SAMPLES.out" << '\n';
    return 1;
  }
  std::string out_path(argv[1]);
  out_path += ".pb";

  std::ifstream ifs;
  ifs.open(argv[1]);

  auto pprof = samples_to_profile(std::move(ifs));
  if (pprof.is_error()) {
    std::cout << "Writing samples failed: \n";
    std::cout << pprof.error_value() << '\n';
    return 1;
  }
  std::ofstream output(out_path.c_str(), std::ios::out);
  pprof.value().SerializePartialToOstream(&output);
}
