// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
	"io"
)

type Renderer interface {
	RenderRuleInfo(*pb.RuleInfo, io.Writer) error
}
