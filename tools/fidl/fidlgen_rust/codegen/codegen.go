// Copyright 2018 The Fuchsia Authors. All rights reserved.
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

func NewGenerator(rustfmtPath, rustfmtConfigPath string, fdomain bool) *Generator {
	var args []string
	if rustfmtConfigPath != "" {
		args = append(args, "--config-path", rustfmtConfigPath)
	}
	formatter := fidlgen.NewFormatter(rustfmtPath, args...)

	return &Generator{
		fdomain: fdomain,
		Generator: fidlgen.NewGenerator("RustTemplates", templates, formatter, template.FuncMap{
			"ResourceDialect": func() string {
				if fdomain {
					return "fdomain_client::fidl::FDomainResourceDialect"
				} else {
					return "fidl::encoding::DefaultFuchsiaResourceDialect"
				}
			},
			"FDomain": func() bool { return fdomain },
			"MarkerNamespace": func() string {
				if fdomain {
					return "fdomain_client::fidl"
				} else {
					return "fidl::endpoints"
				}
			},
			"ChannelType": func() string {
				if fdomain {
					return "fdomain_client::Channel"
				} else {
					return "::fidl::AsyncChannel"
				}
			},
		})}
}

func (gen *Generator) GenerateFidl(ir fidlgen.Root, outputFilename string, includeDrivers bool) error {
	tree := Compile(ir, includeDrivers, gen.fdomain)
	return gen.GenerateFile(outputFilename, "GenerateSourceFile", tree)
}
