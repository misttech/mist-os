// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/fidlgen_hlcpp/codegen"
	cpp "go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen_cpp"
)

func main() {
	flags := cpp.NewCmdlineFlags("hlcpp", []string{})
	fidl := flags.ParseAndLoadIR()
	root := cpp.Compile(fidl)
	generator := codegen.NewGenerator(flags, template.FuncMap{})
	generator.GenerateFiles(root, []string{"Header", "Implementation", "TestBase", "CodingTables"})
}
