// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/fix/command_line_options.h"

#include <lib/cmdline/args_parser.h>

#include <iostream>

namespace fidl::fix {

namespace help {

// Appears at the top of the --help output above the switch list.
const char kArgSpec[] = "--fix <fix-name> [OPTIONS...] file1.fidl [fileN.fidl...]";
const char kIntro[] = R"(
OPTIONS:
)";

const char kFix[] = R"(  --fix=<fix-name>
   -f This required flag specifies the fix that is to be applied:
        fidl-fix --fix=foo lib.fidl)";
const char kExperiments[] = R"(  --experimental=<experiment-name>
   -e If present, this flag enables an experimental feature of fidlc. Some
      fixes require one or more such flags to be enabled. The flag may be
      passed multiple times:
        fidl-fix --fix=foo --experimental=a --experimental=b lib.fidl)";
const char kAvailable[] = R"(  --available=<platform>:<version>
   -a If present, this flag selects a version for a platform. The flag may be
      passed multiple times:
        fidl-fix --fix=foo --available=bar:1 --available=baz:2 lib.fidl)";
const char kDep[] = R"(  --dep=<dep1[,depN...]>
   -d If present, each `--dep` options specifies a comma-separated list of
      files representing a single dependency library. Dependencies should be
      supplied in topologically sorted order, with subsequent dependencies
      able to reference their predecessors:
        fidl-fix --fix=foo --dep=1a.fidl,1b.fidl --dep=2.fidl lib.fidl)";
const char kHelp[] = R"(  --help
   -h Print this help message.)";

}  // namespace help

std::string Usage(const std::string& argv0) {
  return argv0 + " " + help::kArgSpec +
         "\n(--help for more details))\n"
         "\n"
         "Returns exit status 0 if fixes were performed successfully, 1 if\n"
         "build errors occurred before fixing, 2 if build errors occurred\n"
         "during fixing, 3 if the build error occurs after fixing, or 4 for\n"
         "all other errors.";
}

cmdline::Status ParseCommandLine(int argc, const char* argv[], CommandLineOptions* options,
                                 std::vector<std::string>* params) {
  std::stringstream suggestion;
  suggestion << "Try: " << argv[0] << " --help";
  if (argc == 1) {
    return cmdline::Status::Error(suggestion.str());
  }

  cmdline::ArgsParser<CommandLineOptions> parser;

  parser.AddSwitch("fix", 'f', help::kFix, &CommandLineOptions::fix);
  parser.AddSwitch("experimental", 'e', help::kExperiments, &CommandLineOptions::experiments);
  parser.AddSwitch("available", 'a', help::kAvailable, &CommandLineOptions::available);
  parser.AddSwitch("dep", 'd', help::kDep, &CommandLineOptions::deps);

  // Special --help switch which doesn't exist in the options structure.
  bool requested_help = false;
  parser.AddGeneralSwitch("help", 'h', help::kHelp, [&requested_help]() { requested_help = true; });

  cmdline::Status status = parser.Parse(argc, argv, options, params);
  if (status.has_error()) {
    return status;
  }

  // Handle --help switch since we're the one that knows about the switches.
  if (requested_help) {
    return cmdline::Status::Error(Usage(argv[0]) + "\n" + help::kIntro + parser.GetHelp());
  }

  return cmdline::Status::Ok();
}

}  // namespace fidl::fix
