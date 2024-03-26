// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"flag"
	"io"
	"log"
	"os"
	"path/filepath"

	"google.golang.org/protobuf/encoding/prototext"

	"go.fuchsia.dev/fuchsia/tools/bazel-docgen"
	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
)

type docGenFlags struct {
	outDir string
	proto  string
}

func parseFlags() docGenFlags {
	var flags docGenFlags
	flag.StringVar(&flags.outDir, "out_dir", "", "path to a directory which will contain output files.")
	flag.StringVar(&flags.proto, "proto", "", "path to a protobuf, as a textproto, file which contains the docs")
	flag.Parse()

	return flags
}

func main() {
	flags := parseFlags()

	bytes, err := os.ReadFile(flags.proto)
	if err != nil {
		log.Fatalln("Error reading file:", err)
	}

	var root pb.ModuleInfo
	if err := prototext.Unmarshal(bytes, &root); err != nil {
		log.Fatalln("Failed to parse module info:", err)
	}

	generator := bazel_docgen.NewDocGenerator()
	renderer := bazel_docgen.NewMarkdownRenderer()

	// Create a simple file creation function which just creates a file. In the
	// future it will be easier to use the zip writer to create one zip file and
	// then unzip into the outDir for testing if it is specified.
	makeFileFn := func(s string) io.Writer {
		f, err := os.Create(filepath.Join(flags.outDir, s))
		if err != nil {
			log.Fatalln("Failed to create new file:", err)
		}
		return f
	}

	generator.RenderModuleInfo(root, &renderer, makeFileFn)
}
