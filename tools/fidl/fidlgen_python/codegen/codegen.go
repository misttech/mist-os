// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"embed"
	"fmt"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

//go:embed *.tmpl
var templates embed.FS

type Generator struct {
	fdomain bool
	*fidlgen.Generator
}

func EscapeQuotes(s string) string {
	// Replace backslashes first to avoid double-escaping
	s = strings.ReplaceAll(s, "\\", "\\\\")
	s = strings.ReplaceAll(s, "\"", "\\\"")
	s = strings.ReplaceAll(s, "'", "\\'")
	s = strings.ReplaceAll(s, "\x00", "\\x00")
	return s
}

func IndentNonEmpty4(s string) string {
	if s == "" {
		return ""
	}
	return fmt.Sprintf("    %s", s)
}

func IndentNonEmpty8(s string) string {
	if s == "" {
		return ""
	}
	return fmt.Sprintf("        %s", s)
}

func NewGenerator(blackPath string, pyprojectTomlPath string) *Generator {
	formatter := fidlgen.NewFormatter(blackPath, "--quiet", "--config", pyprojectTomlPath, "-")

	return &Generator{
		Generator: fidlgen.NewGenerator("PythonTemplates", templates, formatter, template.FuncMap{
			"escapeQuotes":    EscapeQuotes,
			"trimSpace":       strings.TrimSpace,
			"indentNonEmpty4": IndentNonEmpty4,
			"indentNonEmpty8": IndentNonEmpty8,
		})}
}

func (gen *Generator) GenerateFidl(ir fidlgen.Root, outputFilename string) error {
	tree := Compile(ir)
	return gen.GenerateFile(outputFilename, "GenerateSourceFile", tree)
}
