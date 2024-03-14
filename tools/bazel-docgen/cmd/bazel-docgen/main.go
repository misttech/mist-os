// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"flag"
	"log"
	"os"

	"google.golang.org/protobuf/encoding/prototext"

	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
)

type docGenFlags struct {
	outputFile string
	proto      string
}

func parseFlags() docGenFlags {
	var flags docGenFlags
	flag.StringVar(&flags.outputFile, "output", "", "path to a file which will contain the output")
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

	d1 := []byte("STAMP\n")
	os.WriteFile(flags.outputFile, d1, 0644)
}
