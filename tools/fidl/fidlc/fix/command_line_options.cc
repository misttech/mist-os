// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/fix/command_line_options.h"

#include <lib/cmdline/args_parser.h>

#include <iostream>

#include "src/lib/fxl/strings/split_string.h"

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

cmdline::Status ProcessCommandLine(fidl::fix::CommandLineOptions& options,
                                   std::vector<std::string>& filepaths,
                                   std::unique_ptr<fidl::fix::Fix>& fix) {
  std::ostringstream error_message;

  if (options.fix.empty()) {
    return cmdline::Status::Error("No --fix argument provided\n");
  }
  if (filepaths.empty()) {
    return cmdline::Status::Error("No files provided\n");
  }

  // Process the fix name.
  std::optional<fidl::Fixable> fixable = fidl::Fixable::Get(options.fix);
  if (!fixable.has_value()) {
    error_message << "Unknown --fix: " << options.fix;
    return cmdline::Status::Error(error_message.str());
  }

  // Process library filepaths.
  auto library = std::make_unique<fidl::SourceManager>();
  for (const auto& filepath : filepaths) {
    if (!library->CreateSource(filepath)) {
      error_message << "Couldn't read in source data from " << filepath;
      return cmdline::Status::Error(error_message.str());
    }
  }

  // Process dependency filepaths.
  std::vector<std::unique_ptr<fidl::SourceManager>> dependencies;
  for (const auto& filepaths : options.deps) {
    dependencies.emplace_back(std::make_unique<fidl::SourceManager>());
    std::unique_ptr<fidl::SourceManager>& dep_manager = dependencies.back();
    std::vector<std::string> filepaths_split =
        fxl::SplitStringCopy(filepaths, ",", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    for (const std::string& filepath : filepaths_split) {
      if (!dep_manager->CreateSource(filepath)) {
        error_message << "Couldn't read in source data from " << filepath;
        return cmdline::Status::Error(error_message.str());
      }
    }
  }

  // Process experimental flags.
  fidl::ExperimentalFlags experimental_flags;
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  for (const auto& experiment : options.experiments) {
    if (!experimental_flags.EnableFlagByName(experiment)) {
      error_message << "Unknown --experimental: " << experiment.c_str();
      return cmdline::Status::Error(error_message.str());
    }
  }

  // Process --available flags.
  fidl::VersionSelection version_selection;
  for (const auto& available : options.available) {
    const auto colon_idx = available.find(':');
    if (colon_idx == std::string::npos) {
      error_message << "Invalid --available argument: " << available.c_str();
      return cmdline::Status::Error(error_message.str());
    }
    const auto platform_str = available.substr(0, colon_idx);
    const auto version_str = available.substr(colon_idx + 1);
    const auto platform = fidl::Platform::Parse(platform_str);
    const auto version = fidl::Version::Parse(version_str);
    if (!platform.has_value()) {
      error_message << "Invalid platform name: " << platform_str.c_str();
      return cmdline::Status::Error(error_message.str());
    }
    if (!version.has_value()) {
      error_message << "Invalid version: " << version_str.c_str();
      return cmdline::Status::Error(error_message.str());
    }
    version_selection.Insert(platform.value(), version.value());
  }

  //  std::unique_ptr<fidl::fix::Fix> fix;
  switch (fixable.value().kind) {
    case fidl::Fixable::Kind::kNoop: {
      fix = std::make_unique<fidl::fix::NoopParsedFix>(std::move(library), experimental_flags);
      break;
    }
    case fidl::Fixable::Kind::kProtocolModifier: {
      fix =
          std::make_unique<fidl::fix::ProtocolModifierFix>(std::move(library), experimental_flags);
      break;
    }
    case fidl::Fixable::Kind::kEmptyStructResponse: {
      fix = std::make_unique<fidl::fix::EmptyStructResponseFix>(
          std::move(library), std::move(dependencies), &version_selection, experimental_flags);
      break;
    }
  }

  return cmdline::Status::Ok();
}
}  // namespace fidl::fix
