// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_FIDL_SERVER_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_FIDL_SERVER_H_

#include <lib/fidl/cpp/wire/transaction.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/syslog/cpp/macros.h>

#include <string>

namespace forensics::stubs {

template <typename Protocol>
class FidlServer : public fidl::testing::TestBase<Protocol> {
 public:
  virtual ~FidlServer() = default;

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<Protocol> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_FIDL_SERVER_H_
