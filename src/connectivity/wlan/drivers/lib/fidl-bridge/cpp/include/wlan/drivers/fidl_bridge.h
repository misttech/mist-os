// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FIDL_BRIDGE_CPP_INCLUDE_WLAN_DRIVERS_FIDL_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FIDL_BRIDGE_CPP_INCLUDE_WLAN_DRIVERS_FIDL_BRIDGE_H_

#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/stdcompat/source_location.h>

#include <wlan/drivers/log.h>

namespace wlan::drivers::fidl_bridge {

// Retrieve the underlying zx_status_t from a FIDL error, which may be either a domain or framework
// error. This assumes that the type of the domain_error is a zx_status_t, not a domain-specific
// error enum.
template <typename ErrorType>
zx_status_t FidlErrorToStatus(const ErrorType& e) {
  return e.is_domain_error() ? e.domain_error() : e.framework_error().status();
}

// Returns a lambda that can be used as the callback passed to |Then| or |ThenExactlyOnce| on the
// return value from making an async FIDL request over |bridge_client_|.
// The returned lambda will forward the result to the provided |completer|.
template <typename FidlMethod, typename AsyncCompleter>
auto ForwardResult(AsyncCompleter completer,
                   cpp20::source_location loc = cpp20::source_location::current()) {
  // This function can be applied to FIDL protocols on both the channel and driver transports.
  constexpr bool channel_transport =
      std::is_same_v<typename FidlMethod::Protocol::Transport, fidl::internal::ChannelTransport>;
  using ResultType = typename std::conditional_t<channel_transport, fidl::Result<FidlMethod>,
                                                 fdf::Result<FidlMethod>>;

  return [completer = std::move(completer), loc](ResultType& result) mutable {
    ltrace(0, nullptr, "Forwarding result for %s", loc.function_name());
    if (result.is_error()) {
      lerror("Result not ok for %s: %s", loc.function_name(),
             result.error_value().FormatDescription().c_str());
    }

    constexpr bool has_reply = FidlMethod::kHasNonEmptyUserFacingResponse ||
                               FidlMethod::kHasDomainError || FidlMethod::kHasFrameworkError;
    if constexpr (has_reply) {
      completer.Reply(result.map_error([](auto error) { return FidlErrorToStatus(error); }));
    } else {
      completer.Reply();
    }
  };
}

}  // namespace wlan::drivers::fidl_bridge

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FIDL_BRIDGE_CPP_INCLUDE_WLAN_DRIVERS_FIDL_BRIDGE_H_
