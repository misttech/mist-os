// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_FEEDBACK_ATTACHMENTS_PREVIOUS_BOOT_KERNEL_LOG_H_
#define SRC_DEVELOPER_FORENSICS_FEEDBACK_ATTACHMENTS_PREVIOUS_BOOT_KERNEL_LOG_H_

#include <lib/fpromise/promise.h>

#include <optional>
#include <string>

#include "src/developer/forensics/feedback/attachments/provider.h"
#include "src/developer/forensics/feedback/attachments/types.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/redact/redactor.h"

namespace forensics::feedback {

// Retrieves the previous boot kernel log.
class PreviousBootKernelLog : public AttachmentProvider {
 public:
  PreviousBootKernelLog(std::optional<std::string> dlog, RedactorBase* redactor);

  // Returns an immediately ready promise containing the previous boot kernel log.
  ::fpromise::promise<AttachmentValue> Get(uint64_t ticket) override;

  // No-op because collection happens synchronously.
  void ForceCompletion(uint64_t ticket, Error error) override;

 private:
  std::optional<std::string> dlog_;
};

}  // namespace forensics::feedback

#endif  // SRC_DEVELOPER_FORENSICS_FEEDBACK_ATTACHMENTS_PREVIOUS_BOOT_KERNEL_LOG_H_
