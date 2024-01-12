// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/target_channel_provider.h"

#include "src/developer/forensics/feedback/annotations/constants.h"

namespace forensics::feedback {

Annotations TargetChannelToAnnotations::operator()(const std::string& target_channel) {
  return Annotations{
      {kSystemUpdateChannelTargetKey, ErrorOrString(target_channel)},
  };
}

Annotations TargetChannelToAnnotations::operator()(const Error error) {
  return Annotations{
      {kSystemUpdateChannelTargetKey, ErrorOrString(error)},
  };
}

std::set<std::string> TargetChannelProvider::GetKeys() const {
  return {
      kSystemUpdateChannelTargetKey,
  };
}

}  // namespace forensics::feedback
