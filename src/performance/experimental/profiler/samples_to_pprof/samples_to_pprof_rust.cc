// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/compiler.h>

#include <fstream>
#include <iostream>

#include "samples_to_pprof.h"

__BEGIN_CDECLS
int convert(const char* in_file, const char* out_file) __attribute__((visibility("default")));
__END_CDECLS

// C compatible declaration to convert `in_file` to pprof protobuf format and write the output to
// out_file.
//
// Returns 0 on success, or non zero on failure.
int convert(const char* in_file, const char* out_file) {
  if (!in_file || !out_file) {
    return 1;
  }
  std::string out_path(out_file);

  std::ifstream in;
  in.open(in_file);
  std::ofstream out(out_path.c_str(), std::ios::out);

  auto pprof = samples_to_profile(std::move(in));
  if (pprof.is_error()) {
    std::cerr << "Writing samples failed: " << pprof.error_value() << '\n';
    return 1;
  }
  pprof.value().SerializePartialToOstream(&out);
  return 0;
}
