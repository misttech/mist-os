// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	"log"
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
	docType  int
}

func newTocEntry(title string, filename string, docType int) tocEntry {
	return tocEntry{
		Title:   title,
		Path:    "/" + filename,
		docType: docType,
	}
}

func checkDuplicateName(name string, entries map[string]tocEntry) {
	if _, ok := entries[name]; ok {
		log.Fatalln("Detected multiple entries with the same name: ", name)
	}
}

func filterEntries(entries []tocEntry, docType int) []tocEntry {
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

func RenderModuleInfo(roots []pb.ModuleInfo, renderer Renderer, fileProvider FileProvider) {
	fileProvider.Open()

	entries := make(map[string]tocEntry)

	for _, moduleInfo := range roots {

		// Render all of our rules
		for _, rule := range moduleInfo.GetRuleInfo() {
			fileName := "rule_" + rule.RuleName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderRuleInfo(rule, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(rule.RuleName, fileName, docTypeRule)
		}

		// Render all of our providers
		for _, provider := range moduleInfo.GetProviderInfo() {
			fileName := "provider_" + provider.ProviderName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderProviderInfo(provider, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(provider.ProviderName, fileName, docTypeProvider)
		}

		// Render all of our starlark functions
		for _, funcInfo := range moduleInfo.GetFuncInfo() {
			fileName := "func_" + funcInfo.FunctionName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderStarlarkFunctionInfo(funcInfo, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(funcInfo.FunctionName, fileName, docTypeStarlarkFunction)
		}

		// Render all of our rules
		for _, repoRule := range moduleInfo.GetRepositoryRuleInfo() {
			fileName := "repo_rule_" + repoRule.RuleName + ".md"
			checkDuplicateName(fileName, entries)
			if err := renderer.RenderRepositoryRuleInfo(repoRule, fileProvider.NewFile(fileName)); err != nil {
				panic(err)
			}
			entries[fileName] = newTocEntry(repoRule.RuleName, fileName, docTypeRepositoryRule)
		}
	}

	// Render our README.md
	readmeWriter := fileProvider.NewFile("README.md")
	readmeWriter.Write([]byte(""))

	tocEntries := maps.Values(entries)
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
