// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/intl_provider.h"

#include <fuchsia/intl/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/zx/time.h>

#include <utility>

namespace forensics::stubs {
namespace {

using fuchsia::intl::LocaleId;
using fuchsia::intl::Profile;
using fuchsia::intl::TimeZoneId;

Profile MakeProfile(const std::optional<std::string>& locale,
                    const std::optional<std::string>& timezone) {
  Profile profile;
  if (locale.has_value()) {
    profile.set_locales({
        LocaleId{
            .id = *locale,
        },
    });
  }

  if (timezone.has_value()) {
    profile.set_time_zones({
        TimeZoneId{
            .id = *timezone,
        },
    });
  }

  return profile;
}

}  // namespace

IntlProvider::IntlProvider(std::optional<std::string> default_locale,
                           std::optional<std::string> default_timezone)
    : locale_(std::move(default_locale)), timezone_(std::move(default_timezone)) {}

void IntlProvider::GetProfile(GetProfileCallback callback) {
  callback(MakeProfile(locale_, timezone_));
}

void IntlProvider::SetLocale(std::string_view locale) {
  locale_ = std::string(locale);
  if (!binding() || !binding()->is_bound()) {
    return;
  }

  binding()->events().OnChange();
}

void IntlProvider::SetTimezone(std::string_view timezone) {
  timezone_ = std::string(timezone);
  if (!binding() || !binding()->is_bound()) {
    return;
  }

  binding()->events().OnChange();
}

IntlProviderDelaysResponse::IntlProviderDelaysResponse(async_dispatcher_t* dispatcher,
                                                       zx::duration delay,
                                                       std::optional<std::string> default_locale,
                                                       std::optional<std::string> default_timezone)
    : dispatcher_(dispatcher),
      delay_(delay),
      locale_(std::move(default_locale)),
      timezone_(std::move(default_timezone)) {}

void IntlProviderDelaysResponse::GetProfile(GetProfileCallback callback) {
  async::PostDelayedTask(
      dispatcher_,
      [locale = locale_, timezone = timezone_, callback = std::move(callback)] {
        callback(MakeProfile(locale, timezone));
      },
      delay_);
}

}  // namespace forensics::stubs
