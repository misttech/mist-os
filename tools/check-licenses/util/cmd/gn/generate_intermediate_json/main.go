// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This binary takes the output of "fx gn gen" (project.json) and removes
// GN targets that aren't in the dependency tree of the given root target
// (--gen_filter_target).
//
// The resulting gen struct object is saved to disk at the location defined
// by "--gen_output".
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/util"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/util/cmd"
)

var (
	genFilterTarget = flag.String("gen_filter_target", "", "Root target used to filter out unrelated projects.")
	genInput        = flag.String("gen_input", "", "Location of project.json file.")
	genOutput       = flag.String("gen_output", "", "Location to save intermediate GN gen output.")
	targetsToRemove = flag.String("targets_to_remove", "", "List of targets (separated by comma) to explicitly remove from the build graph.")
)

func main() {
	var err error
	flag.Parse()

	if *genInput == "" {
		cmd.Exit(fmt.Errorf("--gen_input must be provided."))
	}
	if *genOutput == "" {
		cmd.Exit(fmt.Errorf("--gen_output must be provided."))
	}

	gen, err := util.LoadGen(*genInput)
	if err != nil {
		cmd.Exit(err)
	}

	excludeTargets := make(map[string]bool, 0)
	if len(*targetsToRemove) > 0 {
		for _, s := range strings.Split(*targetsToRemove, ",") {
			if len(s) > 0 {
				excludeTargets[s] = true
			}
		}
	}
	if *genFilterTarget != "" {
		if err := gen.FilterTargetsInDependencyTree(*genFilterTarget, excludeTargets); err != nil {
			cmd.Exit(err)
		}
	}

	var data []byte
	data, err = json.MarshalIndent(gen, "", "  ")
	if err != nil {
		cmd.Exit(err)
	}
	cmd.Exit(cmd.SaveFile(data, *genOutput))
}
