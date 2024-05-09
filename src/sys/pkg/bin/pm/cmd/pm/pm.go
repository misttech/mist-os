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
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/archive"
	buildcmd "go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/build"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/genkey"
	initcmd "go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/init"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/publish"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/seal"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/serve"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/update"
	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/cmd/pm/verify"
)

const usage = `Usage: %s [-k key] [-m manifest] [-o output dir] [-t tempdir] <command> [-help]

IMPORTANT: Please note that pm is being sunset and will be removed.
           Building packages and serving repositories is supported
           through ffx. Please adapt workflows accordingly.

Package Commands:
    init     - initialize a package meta directory in the standard form
    build    - perform update and seal in order
    update   - update the merkle roots in meta/contents
    seal     - seal package metadata into a meta.far
    verify   - verify metadata
    archive  - construct a single .far representation of the package

Repository Commands:
    publish  - publish a package to a local repository
    serve    - serve a local repository

For help with individual commands run "pm <command> --help"
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
		err = archive.Run(cfg, flag.Args()[1:])

	case "build":
		err = buildcmd.Run(cfg, flag.Args()[1:])

	case "delta":
		fmt.Fprintf(os.Stderr, "delta is deprecated without replacement")
		err = nil

	case "expand":
		fmt.Fprintf(os.Stderr, "please use 'ffx package archive extract' instead")
		err = nil

	case "genkey":
		err = genkey.Run(cfg, flag.Args()[1:])

	case "init":
		err = initcmd.Run(cfg, flag.Args()[1:])

	case "publish":
		err = publish.Run(cfg, flag.Args()[1:])

	case "seal":
		err = seal.Run(cfg, flag.Args()[1:])

	case "sign":
		fmt.Fprintf(os.Stderr, "sign is deprecated without replacement")
		err = nil

	case "serve":
		err = serve.Run(cfg, flag.Args()[1:], nil)

	case "snapshot":
		fmt.Fprintf(os.Stderr, "snapshot is deprecated without replacement")
		err = nil

	case "update":
		err = update.Run(cfg, flag.Args()[1:])

	case "verify":
		err = verify.Run(cfg, flag.Args()[1:])

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
