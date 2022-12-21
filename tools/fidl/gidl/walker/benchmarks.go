// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package walker

import (
	"bytes"
	_ "embed"
	"fmt"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/config"
	libhlcpp "go.fuchsia.dev/fuchsia/tools/fidl/gidl/hlcpp"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/ir"
	libllcpp "go.fuchsia.dev/fuchsia/tools/fidl/gidl/llcpp/lib"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

var (
	//go:embed benchmarks.tmpl
	benchmarkTmplText string

	benchmarkTmpl = template.Must(template.New("benchmarkTmpl").Parse(benchmarkTmplText))
)

type benchmark struct {
	Path, Name, Type     string
	ValueBuild, ValueVar string
	HandleDefs           string
}

type benchmarkTmplInput struct {
	FidlLibrary string
	Benchmarks  []benchmark
}

func GenerateBenchmarks(gidl ir.All, fidl fidlgen.Root, config config.GeneratorConfig) ([]byte, error) {
	schema := mixer.BuildSchema(fidl)
	tmplInput := benchmarkTmplInput{
		FidlLibrary: libraryName(config.CppBenchmarksFidlLibrary),
	}
	for _, gidlBenchmark := range gidl.Benchmark {
		decl, err := schema.ExtractDeclaration(gidlBenchmark.Value, gidlBenchmark.HandleDefs)
		if err != nil {
			return nil, fmt.Errorf("walker benchmark %s: %s", gidlBenchmark.Name, err)
		}
		valBuild, valVar := libllcpp.BuildValueUnowned(gidlBenchmark.Value, decl, libllcpp.HandleReprRaw)
		tmplInput.Benchmarks = append(tmplInput.Benchmarks, benchmark{
			Path:       gidlBenchmark.Name,
			Name:       benchmarkName(gidlBenchmark.Name),
			Type:       llcppBenchmarkType(config.CppBenchmarksFidlLibrary, gidlBenchmark.Value),
			ValueBuild: valBuild,
			ValueVar:   valVar,
			HandleDefs: libhlcpp.BuildHandleDefs(gidlBenchmark.HandleDefs),
		})
	}
	var buf bytes.Buffer
	if err := benchmarkTmpl.Execute(&buf, tmplInput); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func libraryName(librarySuffix string) string {
	return fmt.Sprintf("benchmarkfidl%s", strings.ReplaceAll(librarySuffix, " ", ""))
}

func llcppBenchmarkType(librarySuffix string, value ir.Value) string {
	return fmt.Sprintf("test_%s::wire::%s", libraryName(librarySuffix), ir.TypeFromValue(value))
}

func benchmarkName(gidlName string) string {
	return strings.ReplaceAll(gidlName, "/", "_")
}
