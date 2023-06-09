// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_PROGRAM_INVOCATION_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_PROGRAM_INVOCATION_H_

#include <optional>
#include <ostream>
#include <string>
#include <vector>

#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/source_manager.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"

namespace fidl {

// This class stores certain properties about the fidlc invocation that may be useful to report
// back in user facing output at some future time.
class ProgramInvocation {
 public:
  ProgramInvocation() = default;
  ProgramInvocation(std::string binary_path, const ExperimentalFlags experimental_flags,
                    const VersionSelection* version_selection,
                    const std::vector<fidl::SourceManager>* source_managers)
      : populated_(true),
        binary_path_(std::move(binary_path)),
        experimental_flags_(experimental_flags),
        version_selection_(version_selection),
        source_managers_(source_managers) {}

  ProgramInvocation(const ProgramInvocation&) = delete;

  bool is_populated() const { return populated_; }
  const std::string& binary_path() const { return binary_path_; }

  // Returns all of the applied experiment flags as a formatted string.
  std::string ExperimentsAsString(const std::string& prefix, const std::string& seperator) const {
    std::ostringstream out;
    bool empty = true;
    experimental_flags_.ForEach(
        [&](const std::string_view name, ExperimentalFlags::Flag flag, bool active) {
          if (active) {
            if (!empty) {
              out << seperator;
            }
            out << prefix + std::string(name);
            empty = false;
          }
        });
    return out.str();
  }

  std::string VersionSelectionAsString(const std::string& prefix,
                                       const std::string& seperator) const {
    if (version_selection_ == nullptr) {
      return "";
    }
    std::ostringstream out;
    bool empty = true;
    version_selection_->ForEach([&](const Platform& platform, Version version) {
      if (!empty) {
        out << seperator;
      }
      out << prefix << platform.name() << ":" << version.ToString();
      empty = false;
    });
    return out.str();
  }

  // Returns all of the dependency library file paths as a formatted string.
  std::string DependenciesAsString(const std::string& dep_prefix,
                                   const std::string& file_seperator) const {
    if (source_managers_ == nullptr || source_managers_->size() <= 1) {
      return "";
    }

    std::ostringstream out;
    auto deps_end = source_managers_->end() - 1;
    auto deps_it = source_managers_->begin();
    while (deps_it != deps_end) {
      out << dep_prefix;
      const std::vector<std::unique_ptr<fidl::SourceFile>>& sources = deps_it->sources();
      for (size_t i = 0; i < sources.size(); i++) {
        if (i > 0) {
          out << file_seperator;
        }
        out << std::string(sources[i]->filename());
      }
      deps_it++;
    }
    return out.str();
  }

  // Returns all of the compiling library's file paths as a formatted string.
  std::optional<std::string> LibraryFilesAsString(const std::string& seperator) const {
    if (source_managers_ == nullptr || source_managers_->empty()) {
      return std::nullopt;
    }

    std::ostringstream out;
    const std::vector<std::unique_ptr<fidl::SourceFile>>& sources =
        source_managers_->back().sources();
    for (size_t i = 0; i < sources.size(); i++) {
      if (i > 0) {
        out << seperator;
      }
      out << std::string(sources[i]->filename());
    }
    return out.str();
  }

 private:
  const bool populated_ = false;
  const std::string binary_path_;
  const ExperimentalFlags experimental_flags_;
  const VersionSelection* version_selection_ = nullptr;

  // Stores file path sets in the order that they are passed to fidlc. The last entry in this vector
  // is always the library being compiled, while the remaining entries are its ordered dependencies.
  const std::vector<fidl::SourceManager>* source_managers_ = nullptr;
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_PROGRAM_INVOCATION_H_
