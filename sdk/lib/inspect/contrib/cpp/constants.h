// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_INSPECT_CONTRIB_CPP_CONSTANTS_H_
#define LIB_INSPECT_CONTRIB_CPP_CONSTANTS_H_

namespace inspect::contrib {

constexpr char kPathName[] = "moniker";
constexpr char kContentsName[] = "payload";
constexpr char kVersionName[] = "version";
constexpr char kMetadataName[] = "metadata";
constexpr char kMetadataFilename[] = "filename";
constexpr char kMetadataComponentURL[] = "component_url";
constexpr char kMetadataTimestamp[] = "timestamp";
constexpr char kMetadataErrors[] = "errors";
constexpr char kMetadataErrorsMessage[] = "message";

}  // namespace inspect::contrib

#endif  // LIB_INSPECT_CONTRIB_CPP_CONSTANTS_H_
