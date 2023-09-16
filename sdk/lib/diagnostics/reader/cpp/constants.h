// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DIAGNOSTICS_READER_CPP_CONSTANTS_H_
#define LIB_DIAGNOSTICS_READER_CPP_CONSTANTS_H_

namespace diagnostics::reader {

constexpr char kMonikerName[] = "moniker";
constexpr char kPayloadName[] = "payload";
constexpr char kVersionName[] = "version";
constexpr char kMetadataName[] = "metadata";
constexpr char kMetadataFilename[] = "filename";
constexpr char kMetadataComponentURL[] = "component_url";
constexpr char kMetadataTimestamp[] = "timestamp";
constexpr char kMetadataErrors[] = "errors";
constexpr char kMetadataErrorsMessage[] = "message";

constexpr char kMetadataFile[] = "file";
constexpr char kMetadataLine[] = "line";
constexpr char kMetadataPid[] = "pid";
constexpr char kMetadataSeverity[] = "severity";
constexpr char kMetadataTags[] = "tags";
constexpr char kMetadataTid[] = "tid";
constexpr char kPayloadMessage[] = "message";
constexpr char kPayloadRoot[] = "root";
constexpr char kPayloadMessageValue[] = "value";

}  // namespace diagnostics::reader

#endif  // LIB_DIAGNOSTICS_READER_CPP_CONSTANTS_H_
