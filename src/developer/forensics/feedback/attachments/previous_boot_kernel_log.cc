// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/previous_boot_kernel_log.h"

#include <lib/fpromise/promise.h>

#include <optional>
#include <string>

#include "src/developer/forensics/feedback/attachments/types.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::feedback {

PreviousBootKernelLog::PreviousBootKernelLog(std::optional<std::string> dlog,
                                             RedactorBase* redactor) {
  // We have no need for the unredacted dlog.
  if (dlog.has_value()) {
    dlog_ = redactor->Redact(*dlog);
  }
}

::fpromise::promise<AttachmentValue> PreviousBootKernelLog::Get(const uint64_t ticket) {
  AttachmentValue data(Error::kMissingValue);

  if (dlog_.has_value()) {
    data = AttachmentValue(*dlog_);
  }

  return fpromise::make_ok_promise(std::move(data));
}

void PreviousBootKernelLog::ForceCompletion(const uint64_t ticket, const Error error) {}

}  // namespace forensics::feedback
