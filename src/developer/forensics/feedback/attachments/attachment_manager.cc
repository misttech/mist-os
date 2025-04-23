// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/attachment_manager.h"

#include <lib/async/cpp/task.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>

#include <utility>

#include "src/developer/forensics/feedback/attachments/types.h"

namespace forensics::feedback {
namespace {

template <typename T>
void EraseNotAllowlisted(std::map<std::string, T>& c, const std::set<std::string>& allowlist) {
  for (auto it = c.begin(); it != c.end();) {
    if (!allowlist.contains(it->first)) {
      FX_LOGS(INFO) << "Attachment \"" << it->first << "\" not allowlisted, dropping";
      c.erase(it++);
    } else {
      ++it;
    }
  }
}

}  // namespace

AttachmentManager::AttachmentManager(async_dispatcher_t* dispatcher,
                                     const std::set<std::string>& allowlist,
                                     std::map<std::string, AttachmentProvider*> providers)
    : dispatcher_(dispatcher), providers_(std::move(providers)) {
  // Remove any providers that return attachments not in |allowlist_|.
  EraseNotAllowlisted(providers_, allowlist);

  for (const auto& k : allowlist) {
    const auto num_providers = providers_.count(k);

    FX_CHECK(num_providers == 1) << "Attachment \"" << k << "\" collected by " << num_providers
                                 << " providers";
  }
}

::fpromise::promise<Attachments> AttachmentManager::GetAttachments(const zx::duration timeout) {
  std::vector<std::string> keys;
  std::vector<::fpromise::promise<AttachmentValue>> promises;

  const uint64_t ticket = ++next_ticket_;
  for (auto& [k, p] : providers_) {
    keys.push_back(k);
    promises.push_back(p->Get(ticket));
  }

  // Complete the collection after |timeout| elapses
  auto self = weak_factory_.GetWeakPtr();
  async::PostDelayedTask(
      dispatcher_,
      [ticket, self] {
        if (self) {
          for (auto& [k, p] : self->providers_) {
            p->ForceCompletion(ticket, Error::kTimeout);
          }
        }
      },
      timeout);

  auto join = ::fpromise::join_promise_vector(std::move(promises));
  using result_t = decltype(join)::value_type;

  Attachments attachments;

  // Start with the static attachments and the add the dynamically collected values to them.
  return join.and_then([keys, attachments = std::move(attachments)](result_t& results) mutable {
    for (size_t i = 0; i < results.size(); ++i) {
      attachments.insert({keys[i], results[i].take_value()});

      // Consider any attachments without content as missing attachments.
      if (auto& attachment = attachments.at(keys[i]);
          attachment.HasValue() && attachment.Value().empty()) {
        attachment = attachment.HasError() ? attachment.Error() : Error::kMissingValue;
      }
    }

    return ::fpromise::ok(std::move(attachments));
  });
}

}  // namespace forensics::feedback
