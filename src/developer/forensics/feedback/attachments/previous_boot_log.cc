// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/previous_boot_log.h"

#include <lib/async/cpp/task.h>
#include <lib/fpromise/promise.h>

#include <string>

#include "src/lib/files/file.h"
#include "src/lib/files/path.h"

namespace forensics::feedback {

PreviousBootLog::PreviousBootLog(async_dispatcher_t* dispatcher, timekeeper::Clock* clock,
                                 const std::optional<zx::duration> delete_previous_boot_log_at,
                                 std::string path)
    : FileBackedProvider(path),
      dispatcher_(dispatcher),
      clock_(clock),
      is_file_deleted_(false),
      path_(std::move(path)) {
  if (!delete_previous_boot_log_at.has_value()) {
    is_file_deleted_ = true;
    return;
  }

  auto self = weak_factory_.GetWeakPtr();
  async::PostDelayedTask(
      dispatcher_,
      [self] {
        if (self) {
          FX_LOGS(INFO) << "Deleting previous boot logs after 24 hours of device uptime";
          self->is_file_deleted_ = true;
          files::DeletePath(self->path_, /*recursive=*/true);
        }
      },
      // The previous boot logs are deleted after |delete_previous_boot_log_at| of device uptime,
      // not component uptime.
      *delete_previous_boot_log_at - zx::nsec(clock_->Now().get()));
}

::fpromise::promise<AttachmentValue> PreviousBootLog::Get(const uint64_t ticket) {
  AttachmentValue previous_boot_log(Error::kNotSet);
  if (is_file_deleted_) {
    return fpromise::make_ok_promise(AttachmentValue(Error::kCustom));
  }

  return FileBackedProvider::Get(ticket);
}

}  // namespace forensics::feedback
