// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"runtime/trace"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
)

const usage = `Usage: %s [-k key] [-m manifest] [-o output dir] [-t tempdir] <command> [-help]

IMPORTANT: Please note that pm is being sunset and will be removed.
           Building packages and serving repositories is supported
           through ffx. Please adapt workflows accordingly.
`

var tracePath = flag.String("trace", "", "write runtime trace to `file`")

func doMain() int {
	cfg := build.NewConfig()
	cfg.InitFlags(flag.CommandLine)

	flag.Usage = func() {
		fmt.Fprintf(os.Stderr, usage, filepath.Base(os.Args[0]))
		fmt.Fprintln(os.Stderr)
		flag.PrintDefaults()
	}

	flag.Parse()

	if *tracePath != "" {
		tracef, err := os.Create(*tracePath)
		if err != nil {
			log.Fatal(err)
		}
		defer func() {
			if err := tracef.Sync(); err != nil {
				log.Fatal(err)
			}
			if err := tracef.Close(); err != nil {
				log.Fatal(err)
			}
		}()
		if err := trace.Start(tracef); err != nil {
			log.Fatal(err)
		}
		defer trace.Stop()
	}

	var err error
	switch flag.Arg(0) {
	case "archive":
		fmt.Fprintf(os.Stderr, "please use 'ffx package archive' instead")
		err = nil

	case "build":
		fmt.Fprintf(os.Stderr, "please use 'ffx package build' instead")
		err = nil

	case "delta":
		fmt.Fprintf(os.Stderr, "delta is deprecated without replacement")
		err = nil

	case "expand":
		fmt.Fprintf(os.Stderr, "please use 'ffx package archive extract' instead")
		err = nil

	case "genkey":
		fmt.Fprintf(os.Stderr, "genkey is deprecated without replacement")
		err = nil

	case "init":
		url := "https://fuchsia.dev/fuchsia-src/development/idk/documentation/packages"
		fmt.Fprintf(os.Stderr, "please create the meta directory and the meta package file according to %v", url)
		err = nil

	case "publish":
		fmt.Fprintf(os.Stderr, "please use 'ffx repository publish' instead")
		err = nil

	case "seal":
		fmt.Fprintf(os.Stderr, "please use 'ffx package far create' instead")
		err = nil

	case "sign":
		fmt.Fprintf(os.Stderr, "sign is deprecated without replacement")
		err = nil

	case "serve":
		fmt.Fprintf(os.Stderr, "please use 'ffx repository serve' instead")
		err = nil

	case "snapshot":
		fmt.Fprintf(os.Stderr, "snapshot is deprecated without replacement")
		err = nil

	case "update":
		fmt.Fprintf(os.Stderr, "update is deprecated without replacement")
		err = nil

	case "verify":
		fmt.Fprintf(os.Stderr, "verify is deprecated without replacement")
		err = nil

	case "newrepo":
		fmt.Fprintf(os.Stderr, "please use 'ffx repository create' instead")
		err = nil

	default:
		flag.Usage()
		return 1
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "%s\n", err)
		return 1
	}

	return 0
}

func main() {
	// we want to use defer in main, but os.Exit doesn't run defers, so...
	os.Exit(doMain())
}
