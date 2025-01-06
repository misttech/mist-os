// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/intl_provider.h"

#include <lib/async/cpp/task.h>
#include <lib/fit/function.h>

#include <memory>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/lib/backoff/backoff.h"

namespace forensics::feedback {

IntlProvider::IntlProvider(async_dispatcher_t* dispatcher,
                           std::shared_ptr<sys::ServiceDirectory> services,
                           std::unique_ptr<backoff::Backoff> backoff)
    : dispatcher_(dispatcher), services_(services), backoff_(std::move(backoff)) {
  services_->Connect(property_provider_ptr_.NewRequest(dispatcher_));
  property_provider_ptr_.events().OnChange =
      ::fit::bind_member<&IntlProvider::GetInternationalization>(this);

  property_provider_ptr_.set_error_handler([this](const zx_status_t status) {
    if (status == ZX_ERR_UNAVAILABLE || status == ZX_ERR_NOT_FOUND) {
      Annotations annotations;
      for (const std::string& key : GetKeys()) {
        annotations.insert({key, ErrorOrString(Error::kNotAvailableInProduct)});
      }

      on_update_(annotations);
      return;
    }

    FX_PLOGS(WARNING, status) << "Lost connection to fuchsia.intl.PropertyProvider";

    auto self = ptr_factory_.GetWeakPtr();
    async::PostDelayedTask(
        dispatcher_,
        [self] {
          if (self) {
            self->services_->Connect(self->property_provider_ptr_.NewRequest(self->dispatcher_));
            self->GetInternationalization();
          }
        },
        backoff_->GetNext());
  });

  GetInternationalization();
}

std::set<std::string> IntlProvider::GetKeys() const {
  return {
      kSystemLocalePrimaryKey,
      kSystemTimezonePrimaryKey,
  };
}

void IntlProvider::OnUpdate() {
  Annotations annotations;

  if (locale_.has_value()) {
    annotations.insert({kSystemLocalePrimaryKey, ErrorOrString(*locale_)});
  }

  if (timezone_.has_value()) {
    annotations.insert({kSystemTimezonePrimaryKey, ErrorOrString(*timezone_)});
  }

  if (!annotations.empty()) {
    on_update_(annotations);
  }
}

void IntlProvider::GetOnUpdate(::fit::function<void(Annotations)> callback) {
  on_update_ = std::move(callback);

  OnUpdate();
}

void IntlProvider::GetInternationalization() {
  FX_CHECK(property_provider_ptr_.is_bound());

  property_provider_ptr_->GetProfile([this](const fuchsia::intl::Profile profile) {
    if (profile.has_locales()) {
      if (const auto& locales = profile.locales(); !locales.empty()) {
        locale_ = locales.front().id;
      }
    }

    if (profile.has_time_zones()) {
      if (const auto& time_zones = profile.time_zones(); !time_zones.empty()) {
        timezone_ = time_zones.front().id;
      }
    }

    OnUpdate();
  });
}

}  // namespace forensics::feedback
