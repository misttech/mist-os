// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	tmpls "go.fuchsia.dev/fuchsia/tools/bazel-docgen/templates"
	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
	"io"
)

type MarkdownRenderer struct{}

func NewMarkdownRenderer() MarkdownRenderer {
	return MarkdownRenderer{}
}

func (r *MarkdownRenderer) RenderRuleInfo(rule *pb.RuleInfo, out io.Writer) error {
	template, err := tmpls.NewRuleTemplate()
	if err == nil {
		err = template.Execute(out, rule)
	}
	return err
}
