// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	"path/filepath"
	"sort"

	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
	"golang.org/x/exp/maps"
	"gopkg.in/yaml.v2"
)

type supportedDocType int

const (
	docTypeRule = iota
	docTypeProvider
	docTypeStarlarkFunction
	docTypeRepositoryRule
)

type tocEntry struct {
	Title    string     `yaml:",omitempty"`
	Path     string     `yaml:",omitempty"`
	Heading  string     `yaml:",omitempty"`
	Sections []tocEntry `yaml:"section,omitempty"`
	docType  supportedDocType
}

func newTocEntry(title string, filename string, docType supportedDocType, mkPathFn func(string) string) tocEntry {
	return tocEntry{
		Title:   title,
		Path:    mkPathFn(filename),
		docType: docType,
	}
}

func checkDuplicateName(name string, entries map[string]tocEntry) bool {
	_, ok := entries[name]
	return ok
}

func filterEntries(entries []tocEntry, docType supportedDocType) []tocEntry {
	var filteredEntries []tocEntry
	for _, entry := range entries {
		if entry.docType == docType {
			filteredEntries = append(filteredEntries, entry)
		}
	}

	sort.Slice(filteredEntries, func(i, j int) bool {
		return filteredEntries[i].Title < filteredEntries[j].Title
	})
	return filteredEntries
}

// There are several places in our bazel rules where we wrap a rule with a
// macro. However, these end up as starlark functions in our docs which is not
// what we want. This rule figures out if a starlark function is trying to be a
// macro or an actual function.
func docTypeForStarlarkFunction(f *pb.StarlarkFunctionInfo) supportedDocType {
	params := f.GetParameter()

	if len(params) > 0 && params[0].GetName() == "name" {
		return docTypeRule
	}

	return docTypeStarlarkFunction
}

func RenderModuleInfo(roots []pb.ModuleInfo, renderer Renderer, fileProvider FileProvider, basePath string) {
	fileProvider.Open()

	mkPathFn := func(s string) string {
		return filepath.Join("/", basePath, s)
	}

	entries := make(map[string]tocEntry)

	for _, moduleInfo := range roots {

		// Render all of our rules
		for _, rule := range moduleInfo.GetRuleInfo() {
			fileName := rule.RuleName + ".md"
			if checkDuplicateName(fileName, entries) {
				continue
			}
			if err := renderer.RenderRuleInfo(rule, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(rule.RuleName, fileName, docTypeRule, mkPathFn)
		}

		// Render all of our providers
		for _, provider := range moduleInfo.GetProviderInfo() {
			fileName := provider.ProviderName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderProviderInfo(provider, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(provider.ProviderName, fileName, docTypeProvider, mkPathFn)
		}

		// Render all of our starlark functions
		for _, funcInfo := range moduleInfo.GetFuncInfo() {
			fileName := funcInfo.FunctionName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderStarlarkFunctionInfo(funcInfo, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(funcInfo.FunctionName, fileName, docTypeForStarlarkFunction(funcInfo), mkPathFn)
		}

		// Render all of our rules
		for _, repoRule := range moduleInfo.GetRepositoryRuleInfo() {
			fileName := repoRule.RuleName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderRepositoryRuleInfo(repoRule, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(repoRule.RuleName, fileName, docTypeRepositoryRule, mkPathFn)
		}
	}

	// Render our README.md
	readmeWriter := fileProvider.NewFile("README.md")
	readmeWriter.Write([]byte(""))

	tocEntries := maps.Values(entries)
	toc := []tocEntry{
		{
			Title: "Overview",
			Path:  mkPathFn("README.md"),
		},
		{
			Title: "API",
			Sections: []tocEntry{
				{
					Title:    "Rules",
					Sections: filterEntries(tocEntries, docTypeRule),
				},
				{
					Title:    "Providers",
					Sections: filterEntries(tocEntries, docTypeProvider),
				},
				{
					Title:    "Starlark Functions",
					Sections: filterEntries(tocEntries, docTypeStarlarkFunction),
				},
				{
					Title:    "Repository Rules",
					Sections: filterEntries(tocEntries, docTypeRepositoryRule),
				},
			},
		},
	}

	// Make our table of contents
	if yamlData, err := yaml.Marshal(&map[string]interface{}{"toc": toc}); err == nil {
		tocWriter := fileProvider.NewFile("_toc.yaml")
		tocWriter.Write(yamlData)
	}

	fileProvider.Close()
}
