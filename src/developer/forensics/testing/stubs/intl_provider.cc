// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/intl_provider.h"

#include <fuchsia/intl/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/zx/time.h>

namespace forensics::stubs {
namespace {

using fuchsia::intl::Profile;
using fuchsia::intl::TimeZoneId;

Profile MakeProfile(std::string_view timezone) {
  Profile profile;
  profile.set_time_zones({
      TimeZoneId{
          .id = std::string(timezone),
      },
  });

  return profile;
}

}  // namespace

IntlProvider::IntlProvider(std::string_view default_timezone) : timezone_(default_timezone) {}

void IntlProvider::GetProfile(GetProfileCallback callback) { callback(MakeProfile(timezone_)); }

void IntlProvider::SetTimezone(std::string_view timezone) {
  timezone_ = std::string(timezone);
  if (!binding() || !binding()->is_bound()) {
    return;
  }

  binding()->events().OnChange();
}

IntlProviderDelaysResponse::IntlProviderDelaysResponse(async_dispatcher_t* dispatcher,
                                                       zx::duration delay,
                                                       std::string_view default_timezone)
    : dispatcher_(dispatcher), delay_(delay), timezone_(default_timezone) {}

void IntlProviderDelaysResponse::GetProfile(GetProfileCallback callback) {
  async::PostDelayedTask(
      dispatcher_,
      [timezone = timezone_, callback = std::move(callback)] { callback(MakeProfile(timezone)); },
      delay_);
}

}  // namespace forensics::stubs
