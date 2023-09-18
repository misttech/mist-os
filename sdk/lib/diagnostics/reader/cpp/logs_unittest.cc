// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/diagnostics/reader/cpp/logs.h>

#include <optional>

#include <rapidjson/document.h>
#include <zxtest/zxtest.h>

namespace {

using diagnostics::reader::LogsData;

TEST(LogsDataTest, ErrorExtraction) {
  std::vector<diagnostics::reader::LogsData> data;
  rapidjson::Document doc;
  doc.Parse(R"({
      "moniker": "moniker",
      "version": 1,
      "data_source": "Logs",
      "payload": {
       "root": {
           "message":{
               "value":"test"
           }
       }
      },
      "metadata": {
        "component_url": "url",
        "errors": [
          {"dropped_logs": {"count": 2}},
          {"rolled_out_logs": {"count": 4}},
          {"parse_record": "bad format"},
          {"other": {"message": "something unexpected happened"}}
        ],
        "file": "test file.cc",
        "line": 420,
        "pid": 1001,
        "severity": "INFO",
        "tags": ["You're", "IT!"],
        "tid": 200,
        "timestamp": 0
      }
    })");
  auto log = LogsData(std::move(doc));
  EXPECT_EQ(log.metadata().errors.size(), 4);

  auto& error = std::get<LogsData::DroppedLogsError>(log.metadata().errors[0]);
  EXPECT_EQ(error.count, 2);

  auto& error2 = std::get<LogsData::RolledOutLogsError>(log.metadata().errors[1]);
  EXPECT_EQ(error2.count, 4);

  auto& error3 = std::get<LogsData::FailedToParseRecordError>(log.metadata().errors[2]);
  EXPECT_EQ(error3.message, "bad format");

  auto& error4 = std::get<LogsData::OtherError>(log.metadata().errors[3]);
  EXPECT_EQ(error4.message, "something unexpected happened");
}

}  // namespace
