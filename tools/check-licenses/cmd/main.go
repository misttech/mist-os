// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

const (
	defaultTarget = "//:default"

	PERMISSIONS_ALLRW_OWNERX = 0755
)

var (
	Config *CheckLicensesConfig

	target = "//:default"
)

var (
	configFile     = flag.String("config_file", "{FUCHSIA_DIR}/tools/check-licenses/cmd/_config.json", "Root config file path.")
	fuchsiaDir     = flag.String("fuchsia_dir", os.Getenv("FUCHSIA_DIR"), "Location of the fuchsia root directory (//).")
	buildDir       = flag.String("build_dir", os.Getenv("FUCHSIA_BUILD_DIR"), "Location of GN build directory.")
	outDir         = flag.String("out_dir", "/tmp/check-licenses", "Directory to write outputs to.")
	licensesOutDir = flag.String("licenses_out_dir", "", "Directory to write license text segments.")

	gnPath              = flag.String("gn_path", "{FUCHSIA_DIR}/prebuilt/third_party/gn/linux-x64/gn", "Path to GN executable. Required when gen_filter_target is specified.")
	genProjectFile      = flag.String("gen_project_file", "{BUILD_DIR}/project.json", "Path to 'project.json' output file.")
	genIntermediateFile = flag.String("gen_intermediate_file", "", "Path to intermediate serialized gen struct.")
	pruneTargets        = flag.String("prune_targets", "", "Flag for filtering out targets from the dependency tree. Targets separated by comma.")

	checkURLs            = flag.Bool("check_urls", false, "Flag for enabling checks for license URLs.")
	overwriteReadmeFiles = flag.Bool("overwrite_readme_files", false, "Flag for enabling README.fuchsia file overwites.")

	outputLicenseFile = flag.Bool("output_license_file", true, "Flag for enabling template expansions.")
	runAnalysis       = flag.Bool("run_analysis", true, "Flag for enabling license analysis and 'result' package tests.")
)

func mainImpl() error {
	var err error

	flag.Parse()

	// fuchsiaDir
	if *fuchsiaDir == "" {
		*fuchsiaDir = "."
	}
	if *fuchsiaDir, err = filepath.Abs(*fuchsiaDir); err != nil {
		return fmt.Errorf("Failed to get absolute directory for *fuchsiaDir %s: %w", *fuchsiaDir, err)
	}
	ConfigVars["{FUCHSIA_DIR}"] = *fuchsiaDir

	// buildDir
	if len(*buildDir) > 0 {
		if *buildDir, err = filepath.Abs(*buildDir); err != nil {
			return fmt.Errorf("Failed to get absolute directory for *buildDir %s: %w", *buildDir, err)
		}
	}
	ConfigVars["{BUILD_DIR}"] = *buildDir

	// outDir
	rootOutDir := *outDir
	productBoard := ""
	if len(*outDir) > 0 {
		*outDir, err = filepath.Abs(*outDir)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *outDir %s: %w", *outDir, err)
		}
		rootOutDir = *outDir
	}
	if _, err := os.Stat(*outDir); os.IsNotExist(err) {
		err := os.MkdirAll(*outDir, PERMISSIONS_ALLRW_OWNERX)
		if err != nil {
			return fmt.Errorf("Failed to create out directory [%s]: %w\n", outDir, err)
		}
	}
	ConfigVars["{OUT_DIR}"] = *outDir
	ConfigVars["{ROOT_OUT_DIR}"] = rootOutDir
	ConfigVars["{PRODUCT_BOARD}"] = productBoard

	// licensesOutDir
	if *licensesOutDir != "" {
		*licensesOutDir, err = filepath.Abs(*licensesOutDir)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *licensesOutDir %s: %w", *licensesOutDir, err)
		}
		if _, err := os.Stat(*licensesOutDir); os.IsNotExist(err) {
			err := os.MkdirAll(*licensesOutDir, PERMISSIONS_ALLRW_OWNERX)
			if err != nil {
				return fmt.Errorf("Failed to create licenses out directory [%s]: %w\n", licensesOutDir, err)
			}
		}
	}
	ConfigVars["{LICENSES_OUT_DIR}"] = *licensesOutDir

	// gnPath
	if len(*gnPath) > 0 {
		*gnPath = strings.ReplaceAll(*gnPath, "{FUCHSIA_DIR}", *fuchsiaDir)
		*gnPath, err = filepath.Abs(*gnPath)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *gnPath %s: %w", *gnPath, err)
		}
	}

	if len(*genIntermediateFile) > 0 {
		*genIntermediateFile = strings.ReplaceAll(*genIntermediateFile, "{BUILD_DIR}", *buildDir)
		*genIntermediateFile, err = filepath.Abs(*genIntermediateFile)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *genIntermediateFile %s: %w", *genIntermediateFile, err)
		}
	}
	*genProjectFile = strings.ReplaceAll(*genProjectFile, "{BUILD_DIR}", *buildDir)
	*genProjectFile, err = filepath.Abs(*genProjectFile)
	if err != nil {
		return fmt.Errorf("Failed to get absolute directory for *genProjectFile %s: %w", *genProjectFile, err)
	}

	pruneTargetsList := strings.Split(*pruneTargets, ",")
	pruneTargetsMap := make(map[string]bool)
	for _, t := range pruneTargetsList {
		if len(t) > 0 {
			pruneTargetsMap[t] = true
		}
	}
	pruneTargetsJSON, err := json.Marshal(pruneTargetsMap)
	if err != nil {
		return fmt.Errorf("Failed to serialize pruneTargets flag %s: %w", *pruneTargets, err)
	}

	ConfigVars["{GN_PATH}"] = *gnPath
	ConfigVars["{GEN_INTERMEDIATE_FILE}"] = *genIntermediateFile
	ConfigVars["{GEN_PROJECT_FILE}"] = *genProjectFile
	ConfigVars["{OUTPUT_LICENSE_FILE}"] = strconv.FormatBool(*outputLicenseFile)
	ConfigVars["{RUN_ANALYSIS}"] = strconv.FormatBool(*runAnalysis)
	ConfigVars["{PRUNE_TARGETS}"] = string(pruneTargetsJSON)

	ConfigVars["{CHECK_URLS}"] = strconv.FormatBool(*checkURLs)
	ConfigVars["{OVERWRITE_README_FILES}"] = strconv.FormatBool(*overwriteReadmeFiles)

	// target
	if flag.NArg() > 1 {
		return fmt.Errorf("check-licenses takes a maximum of 1 positional argument (filepath or gn target), got %v\n", flag.NArg())
	}
	if flag.NArg() == 1 {
		target = flag.Arg(0)
	}
	ConfigVars["{TARGET}"] = target

	spdxDocName := productBoard
	if target != defaultTarget {
		spdxDocName = target
	}
	ConfigVars["{SPDX_DOC_NAME}"] = spdxDocName

	// configFile
	*configFile = strings.ReplaceAll(*configFile, "{FUCHSIA_DIR}", *fuchsiaDir)
	Config, err = NewCheckLicensesConfig(*configFile)
	if err != nil {
		return err
	}

	if err := os.Chdir(*fuchsiaDir); err != nil {
		return err
	}

	if err := Execute(context.Background()); err != nil {
		return fmt.Errorf("failed to analyze the given directory: %w", err)
	}
	return nil
}

func main() {
	if err := mainImpl(); err != nil {
		fmt.Fprintf(os.Stderr, "check-licenses: %s\nSee go/fuchsia-licenses-playbook for information on resolving common errors.\n", err)
		os.Exit(1)
	}
}
