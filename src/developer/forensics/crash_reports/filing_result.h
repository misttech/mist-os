// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_FILING_RESULT_H_
#define SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_FILING_RESULT_H_

#include <lib/fit/function.h>

#include <string>

namespace forensics::crash_reports {

enum class FilingResult {
  kReportUploaded = 0,
  kReportOnDisk = 1,
  kReportInMemory = 2,
  kReportNotFiledUserOptedOut = 3,
  kInvalidArgsError = 4,
  kServerError = 5,
  kPersistenceError = 6,
};

using FilingResultFn = fit::callback<void(FilingResult, std::string)>;

}  // namespace forensics::crash_reports

#endif  // SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_FILING_RESULT_H_
