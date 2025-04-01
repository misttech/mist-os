// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"embed"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

//go:embed *.tmpl
var templates embed.FS

type Generator struct {
	fdomain bool
	*fidlgen.Generator
}

func NewGenerator() *Generator {
	// TODO(https://fxbug.dev/394423190): Integrate with a formatter. Unfortunately, shac
	// is not compatible with fidlgen.NewFormatter because shac does not format files
	// from stdin.
	formatter := fidlgen.NewFormatter("")

	return &Generator{
		Generator: fidlgen.NewGenerator("PythonTemplates", templates, formatter, template.FuncMap{})}
}

func (gen *Generator) GenerateFidl(ir fidlgen.Root, outputFilename string) error {
	tree := Compile(ir)
	return gen.GenerateFile(outputFilename, "GenerateSourceFile", tree)
}
