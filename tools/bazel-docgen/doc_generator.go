// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	"io"

	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
	"gopkg.in/yaml.v2"
)

type DocGenerator struct {
}

type MakeWriterFn func(name string) io.Writer

func NewDocGenerator() DocGenerator {
	return DocGenerator{}
}

type tocEntry struct {
	Title    string
	Path     string     `yaml:",omitempty"`
	Sections []tocEntry `yaml:"section,omitempty"`
}

func (dc DocGenerator) RenderModuleInfo(moduleInfo pb.ModuleInfo, renderer Renderer, writerFn MakeWriterFn) error {

	// Render all of our rules
	var ruleEntries []tocEntry
	for _, rule := range moduleInfo.GetRuleInfo() {
		file_name := "rule_" + rule.RuleName + ".md"
		if err := renderer.RenderRuleInfo(rule, writerFn(file_name)); err != nil {
			return err
		}
		ruleEntries = append(ruleEntries, tocEntry{
			Title: rule.RuleName,
			Path:  file_name,
		})
	}

	// Render our README.md
	readmeWriter := writerFn("README.md")
	readmeWriter.Write([]byte(""))

	toc := []tocEntry{
		{
			Title: "Overview",
			Path:  "/README.md",
		},
		{
			Title:    "API",
			Sections: ruleEntries,
		},
	}
	// Make our table of contents
	if yamlData, err := yaml.Marshal(&map[string]interface{}{"toc": toc}); err == nil {
		tocWriter := writerFn("_toc.yaml")
		tocWriter.Write(yamlData)
	}

	return nil
}
