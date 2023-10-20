// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/parser"
	"golang.org/x/exp/slices"
)

var corpusPaths = map[ir.OutputType]string{
	ir.OutputTypeConformance: "src/tests/fidl/conformance_suite",
	ir.OutputTypeBenchmark:   "src/tests/benchmarks/fidl/benchmark_suite",
}

type auditFlags struct {
	corpusName *string
	language   *string
}

func (f auditFlags) valid() bool {
	if _, ok := corpusPaths[ir.OutputType(*f.corpusName)]; !ok {
		fmt.Printf("unknown corpus name %s\n", *f.corpusName)
		return false
	}
	if *f.language == "" {
		fmt.Printf("-language must be specified\n")
		return false
	}
	return true
}

var flags = auditFlags{
	corpusName: flag.String("corpus", "conformance", "corpus name (conformance or benchmark)"),
	language:   flag.String("language", "", "language to filter to"),
}

func main() {
	flag.Parse()

	if !flags.valid() {
		flag.PrintDefaults()
		os.Exit(1)
	}

	outputType := ir.OutputType(*flags.corpusName)
	language := ir.Language(*flags.language)

	fuchsiaDir, ok := os.LookupEnv("FUCHSIA_DIR")
	if !ok {
		fmt.Printf("FUCHSIA_DIR environment variable must be set")
		os.Exit(1)
	}

	globPattern := fuchsiaDir + "/" + corpusPaths[outputType] + "/*.gidl"
	gidlFiles, err := filepath.Glob(globPattern)
	if err != nil {
		fmt.Printf("failed to match glob pattern %q when looking for GIDL files\n", globPattern)
		os.Exit(1)
	}
	all := parseAllGidlIr(gidlFiles)
	filtered := filter(all, language)

	fmt.Printf("Disabled tests for %s\n", language)
	fmt.Printf("***************************************\n")

	if len(filtered.EncodeSuccess) > 0 {
		fmt.Printf("\nEncode success\n")
		fmt.Printf("---------------------------------------\n")
		for _, t := range filtered.EncodeSuccess {
			fmt.Printf("%s\n", t.Name)
		}
	}

	if len(filtered.EncodeFailure) > 0 {
		fmt.Printf("\nEncode failure\n")
		fmt.Printf("---------------------------------------\n")
		for _, t := range filtered.EncodeFailure {
			fmt.Printf("%s\n", t.Name)
		}
	}

	if len(filtered.DecodeSuccess) > 0 {
		fmt.Printf("\nDecode success\n")
		fmt.Printf("---------------------------------------\n")
		for _, t := range filtered.DecodeSuccess {
			fmt.Printf("%s\n", t.Name)
		}
	}

	if len(filtered.DecodeFailure) > 0 {
		fmt.Printf("\nDecode failure\n")
		fmt.Printf("---------------------------------------\n")
		for _, t := range filtered.DecodeFailure {
			fmt.Printf("%s\n", t.Name)
		}
	}

	if len(filtered.Benchmark) > 0 {
		fmt.Printf("\nBenchmark\n")
		fmt.Printf("---------------------------------------\n")
		for _, t := range filtered.Benchmark {
			fmt.Printf("%s\n", t.Name)
		}
	}
}

func parseGidlIr(filename string) ir.All {
	f, err := os.Open(filename)
	if err != nil {
		panic(err)
	}
	defer f.Close()
	config := parser.Config{
		Languages:   ir.AllLanguages(),
		WireFormats: ir.AllWireFormats(),
	}
	result, err := parser.NewParser(filename, f, config).Parse()
	if err != nil {
		panic(err)
	}
	return result
}

func parseAllGidlIr(paths []string) ir.All {
	var parsedGidlFiles []ir.All
	for _, path := range paths {
		parsedGidlFiles = append(parsedGidlFiles, parseGidlIr(path))
	}
	return ir.Merge(parsedGidlFiles)
}

// This is the opposite of ir.FilterByLanguage.
func filter(input ir.All, language ir.Language) ir.All {
	shouldKeep := func(allowlist *[]ir.Language, denylist *[]ir.Language) bool {
		if denylist != nil && slices.Contains(*denylist, language) {
			return true
		}
		if allowlist != nil {
			return !slices.Contains(*allowlist, language)
		}
		return false
	}
	var output ir.All
	for _, def := range input.EncodeSuccess {
		if shouldKeep(def.BindingsAllowlist, def.BindingsDenylist) {
			output.EncodeSuccess = append(output.EncodeSuccess, def)
		}
	}
	for _, def := range input.DecodeSuccess {
		if shouldKeep(def.BindingsAllowlist, def.BindingsDenylist) {
			output.DecodeSuccess = append(output.DecodeSuccess, def)
		}
	}
	for _, def := range input.EncodeFailure {
		if shouldKeep(def.BindingsAllowlist, def.BindingsDenylist) {
			output.EncodeFailure = append(output.EncodeFailure, def)
		}
	}
	for _, def := range input.DecodeFailure {
		if shouldKeep(def.BindingsAllowlist, def.BindingsDenylist) {
			output.DecodeFailure = append(output.DecodeFailure, def)
		}
	}
	for _, def := range input.Benchmark {
		if shouldKeep(def.BindingsAllowlist, def.BindingsDenylist) {
			output.Benchmark = append(output.Benchmark, def)
		}
	}
	return output
}
