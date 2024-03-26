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
	Title    string     `yaml:",omitempty"`
	Path     string     `yaml:",omitempty"`
	Heading  string     `yaml:",omitempty"`
	Sections []tocEntry `yaml:"section,omitempty"`
}

func (dc DocGenerator) RenderModuleInfo(moduleInfo pb.ModuleInfo, renderer Renderer, writerFn MakeWriterFn) error {

	// Render all of our rules
	var ruleEntries []tocEntry
	for _, rule := range moduleInfo.GetRuleInfo() {
		file_name := "rule_" + rule.RuleName + ".md"
		if err := renderer.RenderRuleInfo(rule, writerFn(file_name)); err != nil {
			panic(err)
		}
		ruleEntries = append(ruleEntries, tocEntry{
			Title: rule.RuleName,
			Path:  file_name,
		})
	}

	// Render all of our providers
	var providerEntries []tocEntry
	for _, provider := range moduleInfo.GetProviderInfo() {
		file_name := "provider_" + provider.ProviderName + ".md"
		if err := renderer.RenderProviderInfo(provider, writerFn(file_name)); err != nil {
			panic(err)
		}
		providerEntries = append(providerEntries, tocEntry{
			Title: provider.ProviderName,
			Path:  file_name,
		})
	}

	// Render all of our starlark functions
	var starlarkFunctionEntries []tocEntry
	for _, funcInfo := range moduleInfo.GetFuncInfo() {
		file_name := "func_" + funcInfo.FunctionName + ".md"
		if err := renderer.RenderStarlarkFunctionInfo(funcInfo, writerFn(file_name)); err != nil {
			panic(err)
		}
		starlarkFunctionEntries = append(starlarkFunctionEntries, tocEntry{
			Title: funcInfo.FunctionName,
			Path:  file_name,
		})
	}

	// Render all of our rules
	var repoRuleEntries []tocEntry
	for _, repo_rule := range moduleInfo.GetRepositoryRuleInfo() {
		file_name := "repo_rule_" + repo_rule.RuleName + ".md"
		if err := renderer.RenderRepositoryRuleInfo(repo_rule, writerFn(file_name)); err != nil {
			panic(err)
		}
		repoRuleEntries = append(repoRuleEntries, tocEntry{
			Title: repo_rule.RuleName,
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
			Heading: "API",
		},
		{
			Sections: []tocEntry{
				{
					Title:    "Rules",
					Sections: ruleEntries,
				},
				{
					Title:    "Providers",
					Sections: providerEntries,
				},
				{
					Title:    "Starlark Functions",
					Sections: starlarkFunctionEntries,
				},
				{
					Title:    "Repository Rules",
					Sections: repoRuleEntries,
				},
			},
		},
	}
	// Make our table of contents
	if yamlData, err := yaml.Marshal(&map[string]interface{}{"toc": toc}); err == nil {
		tocWriter := writerFn("_toc.yaml")
		tocWriter.Write(yamlData)
	}

	return nil
}
