// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <lib/cmdline/status.h>
#include <stdio.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <fstream>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "src/lib/fxl/strings/split_string.h"
#include "tools/fidl/fidlc/fix/command_line_options.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/fixables.h"
#include "tools/fidl/fidlc/include/fidl/fixes.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_manager.h"

namespace {

constexpr int FixStatusToExitCode(fidl::fix::Status status) {
  switch (status) {
    case fidl::fix::Status::kOk:
    case fidl::fix::Status::kComplete:
      return 0;
    case fidl::fix::Status::kErrorPreFix:
      return 1;
    case fidl::fix::Status::kErrorDuringFix:
      return 2;
    case fidl::fix::Status::kErrorPostFix:
      return 3;
    case fidl::fix::Status::kErrorOther:
      return 4;
  }
}

[[noreturn]] void FailWithUsage(fidl::fix::Status kind, const std::string& argv0,
                                const char* message, ...) {
  va_list args;
  va_start(args, message);
  vfprintf(stderr, message, args);
  va_end(args);
  std::cerr << fidl::fix::Usage(argv0) << std::endl;
  exit(FixStatusToExitCode(kind));
}

[[noreturn]] void Fail(fidl::fix::Status kind, const char* message, ...) {
  va_list args;
  va_start(args, message);
  vfprintf(stderr, message, args);
  va_end(args);
  exit(FixStatusToExitCode(kind));
}

}  // namespace

int main(int argc, char* argv[]) {
  fidl::fix::CommandLineOptions options;
  std::vector<std::string> filepaths;
  cmdline::Status status =
      fidl::fix::ParseCommandLine(argc, const_cast<const char**>(argv), &options, &filepaths);
  if (status.has_error()) {
    Fail(fidl::fix::Status::kErrorOther, "%s\n", status.error_message().c_str());
  }

  if (options.fix.empty()) {
    FailWithUsage(fidl::fix::Status::kErrorOther, argv[0], "No --fix argument provided\n");
  }
  if (filepaths.empty()) {
    FailWithUsage(fidl::fix::Status::kErrorOther, argv[0], "No files provided\n");
  }

  // Process the fix name.
  std::optional<fidl::Fixable> fixable = fidl::Fixable::Get(options.fix);
  if (!fixable.has_value()) {
    FailWithUsage(fidl::fix::Status::kErrorOther, argv[0], "Unknown --fix: %s\n", &options.fix);
  }

  // Process library filepaths.
  auto library = std::make_unique<fidl::SourceManager>();
  for (const auto& filepath : filepaths) {
    if (!library->CreateSource(filepath)) {
      Fail(fidl::fix::Status::kErrorOther, "Couldn't read in source data from %s\n",
           filepath.c_str());
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
        Fail(fidl::fix::Status::kErrorOther, "Couldn't read in source data from %s\n",
             filepath.c_str());
      }
    }
  }

  // Process experimental flags.
  fidl::ExperimentalFlags experimental_flags;
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  for (const auto& experiment : options.experiments) {
    if (!experimental_flags.EnableFlagByName(experiment)) {
      FailWithUsage(fidl::fix::Status::kErrorOther, argv[0], "Unknown --experimental: %s\n",
                    std::string_view(experiment));
    }
  }

  std::unique_ptr<fidl::fix::Fix> fix;
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
          std::move(library), std::move(dependencies), experimental_flags);
      break;
    }
  }
  ZX_ASSERT(fix != nullptr);

  fidl::fix::Status validate = fix->ValidateFlags();
  if (validate != fidl::fix::Status::kOk) {
    FailWithUsage(fidl::fix::Status::kErrorOther, argv[0], "Required --experimental flags missing");
  }

  fidl::Reporter reporter;
  reporter.ignore_fixables();
  fidl::fix::TransformResult result = fix->Transform(&reporter);
  if (!result.is_ok()) {
    // If we've reached this point, there has been a failure.
    const fidl::fix::Failure& failure = result.error_value();
    bool enable_color = !std::getenv("NO_COLOR") && isatty(fileno(stderr));

    // Print fix-specific errors first.
    for (const auto& error : failure.errors) {
      printf("%s \n", error.msg.c_str());
    }

    // Then print general fidlc errors.
    reporter.PrintReports(enable_color);

    Fail(failure.status, "Fixing operation failed!\n");
  }

  // Success! Update the files.
  fidl::fix::OutputMap transforms = result.value();
  for (const auto& transform : transforms) {
    FILE* out_file;
    const char* filename = transform.first.c_str();
    const std::string& contents = transform.second;
    out_file = fopen(filename, "w+");
    if (out_file == nullptr) {
      std::string error = "Fail: cannot open file: ";
      error.append(filename);
      error.append(":\n");
      error.append(strerror(errno));
      Fail(fidl::fix::Status::kErrorOther, error.c_str());
    }
    fprintf(out_file, "%s", contents.c_str());
  }

  return 0;
}
