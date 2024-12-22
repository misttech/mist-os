// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/file_backed_provider.h"

#include <lib/fpromise/promise.h>

#include <string>

#include "src/lib/files/file.h"

namespace forensics::feedback {

FileBackedProvider::FileBackedProvider(std::string path) : path_(std::move(path)) {}

::fpromise::promise<AttachmentValue> FileBackedProvider::Get(const uint64_t ticket) {
  AttachmentValue data(Error::kNotSet);

  if (std::string content; files::ReadFileToString(path_, &content)) {
    data = content.empty() ? AttachmentValue(Error::kMissingValue)
                           : AttachmentValue(std::move(content));
  } else {
    FX_LOGS_FIRST_N(WARNING, 1) << "Failed to read: " << path_;
    data = AttachmentValue(Error::kFileReadFailure);
  }

  if (!data.HasValue()) {
    FX_LOGS_FIRST_N(WARNING, 1) << "Failed to build attachment '" << path_
                                << "' due to error: " << ToString(data.Error());
  }
  return fpromise::make_ok_promise(std::move(data));
}

void FileBackedProvider::ForceCompletion(const uint64_t ticket, const Error error) {}

}  // namespace forensics::feedback
