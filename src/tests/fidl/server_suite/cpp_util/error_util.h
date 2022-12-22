// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_TESTS_FIDL_SERVER_SUITE_CPP_UTIL_ERROR_UTIL_H_
#define SRC_TESTS_FIDL_SERVER_SUITE_CPP_UTIL_ERROR_UTIL_H_

#include <fidl/fidl.serversuite/cpp/fidl.h>
#include <lib/fidl/cpp/wire/status.h>

namespace servertest_util {

inline fidl_serversuite::TeardownReason ClassifyError(const fidl::UnbindInfo& info) {
  switch (info.reason()) {
    case fidl::Reason::kUnbind:
    case fidl::Reason::kClose:
    case fidl::Reason::kDispatcherError:
    case fidl::Reason::kTransportError:
    case fidl::Reason::kEncodeError:
      return fidl_serversuite::TeardownReason::kOther;
    case fidl::Reason::kUnexpectedMessage:
      if (info.lossy_description() == ::fidl::internal::kErrorInvalidHeader) {
        return fidl_serversuite::TeardownReason::kDecodingError;
      }
      return fidl_serversuite::TeardownReason::kUnexpectedMessage;
    case fidl::Reason::kPeerClosed:
      return fidl_serversuite::TeardownReason::kChannelPeerClosed;
    case fidl::Reason::kDecodeError:
      return fidl_serversuite::TeardownReason::kDecodingError;
    default:
      auto description = info.FormatDescription();
      ZX_PANIC("UnbindInfo had an unsupported reason: %s", description.c_str());
  }
}

}  // namespace servertest_util

#endif  // SRC_TESTS_FIDL_SERVER_SUITE_CPP_UTIL_ERROR_UTIL_H_
