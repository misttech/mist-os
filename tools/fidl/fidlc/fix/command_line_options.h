// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_FIX_COMMAND_LINE_OPTIONS_H_
#define TOOLS_FIDL_FIDLC_FIX_COMMAND_LINE_OPTIONS_H_

#include <lib/cmdline/status.h>

#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "tools/fidl/fidlc/include/fidl/fixes.h"

namespace fidl::fix {

struct CommandLineOptions {
  std::string fix;
  std::vector<std::string> deps;
  std::vector<std::string> experiments;
  std::vector<std::string> available;
};

// Parses the given command line into options and params.
//
// Returns an error if the command-line is badly formed. In addition, --help
// text will be returned as an error.
cmdline::Status ParseCommandLine(int argc, const char* argv[], CommandLineOptions* options,
                                 std::vector<std::string>* params);

// Returns the fidl-fix usage string.
std::string Usage(const std::string& argv0);

// Create a fix object from the command line options
cmdline::Status ProcessCommandLine(CommandLineOptions& options, std::vector<std::string>& filepaths,
                                   std::unique_ptr<Fix>& fix);

}  // namespace fidl::fix

#endif  // TOOLS_FIDL_FIDLC_FIX_COMMAND_LINE_OPTIONS_H_
