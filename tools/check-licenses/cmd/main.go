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

	buildInfoVersion = flag.String("build_info_version", "version", "Version of fuchsia being built. Used for uploading results.")
	buildInfoProduct = flag.String("build_info_product", "product", "Version of fuchsia being built. Used for uploading results.")
	buildInfoBoard   = flag.String("build_info_board", "board", "Version of fuchsia being built. Used for uploading results.")

	gnPath       = flag.String("gn_path", "{FUCHSIA_DIR}/prebuilt/third_party/gn/linux-x64/gn", "Path to GN executable. Required when target is specified.")
	gnGenFile    = flag.String("gn_gen_file", "{BUILD_DIR}/project.json", "Path to 'project.json' output file.")
	pruneTargets = flag.String("prune_targets", "", "Flag for filtering out targets from the dependency tree. Targets separated by comma.")

	checkURLs = flag.Bool("check_urls", false, "Flag for enabling checks for license URLs.")

	outputLicenseFile = flag.Bool("output_license_file", true, "Flag for enabling template expansions.")
	runAnalysis       = flag.Bool("run_analysis", true, "Flag for enabling license analysis and 'result' package tests.")
)

func mainImpl() error {
	var err error

	flag.Parse()

	// buildInfo
	ConfigVars["{BUILD_INFO_VERSION}"] = *buildInfoVersion

	if *buildInfoProduct == "product" {
		b, err := os.ReadFile(filepath.Join(*buildDir, "product.txt"))
		if err == nil {
			*buildInfoProduct = string(b)
		}
	}
	ConfigVars["{BUILD_INFO_PRODUCT}"] = *buildInfoProduct

	if *buildInfoBoard == "board" {
		b, err := os.ReadFile(filepath.Join(*buildDir, "board.txt"))
		if err == nil {
			*buildInfoBoard = string(b)
		}
	}
	ConfigVars["{BUILD_INFO_BOARD}"] = *buildInfoBoard

	// fuchsiaDir
	if *fuchsiaDir == "" {
		*fuchsiaDir = "."
	}
	if *fuchsiaDir, err = filepath.Abs(*fuchsiaDir); err != nil {
		return fmt.Errorf("Failed to get absolute directory for *fuchsiaDir %v: %v", *fuchsiaDir, err)
	}
	ConfigVars["{FUCHSIA_DIR}"] = *fuchsiaDir

	// buildDir
	if *buildDir == "" && *outputLicenseFile {
		return fmt.Errorf("--build_dir cannot be empty.")
	}
	if *buildDir, err = filepath.Abs(*buildDir); err != nil {
		return fmt.Errorf("Failed to get absolute directory for *buildDir%v: %v", *buildDir, err)
	}
	ConfigVars["{BUILD_DIR}"] = *buildDir

	// outDir
	rootOutDir := *outDir
	productBoard := ""
	if *outDir != "" {
		*outDir, err = filepath.Abs(*outDir)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *outDir %v: %v", *outDir, err)
		}
		rootOutDir = *outDir

		if *outputLicenseFile {
			productBoard = fmt.Sprintf("%v.%v", *buildInfoProduct, *buildInfoBoard)
			*outDir = filepath.Join(*outDir, *buildInfoVersion, productBoard)
		} else {
			*outDir = filepath.Join(*outDir, *buildInfoVersion, "everything")
		}
	}
	if _, err := os.Stat(*outDir); os.IsNotExist(err) {
		err := os.MkdirAll(*outDir, 0755)
		if err != nil {
			return fmt.Errorf("Failed to create out directory [%v]: %v\n", outDir, err)
		}
	}
	ConfigVars["{OUT_DIR}"] = *outDir
	ConfigVars["{ROOT_OUT_DIR}"] = rootOutDir
	ConfigVars["{PRODUCT_BOARD}"] = productBoard

	// licensesOutDir
	if *licensesOutDir != "" {
		*licensesOutDir, err = filepath.Abs(*licensesOutDir)
		if err != nil {
			return fmt.Errorf("Failed to get absolute directory for *licensesOutDir %v: %v", *licensesOutDir, err)
		}
		if _, err := os.Stat(*licensesOutDir); os.IsNotExist(err) {
			err := os.MkdirAll(*licensesOutDir, 0755)
			if err != nil {
				return fmt.Errorf("Failed to create licenses out directory [%v]: %v\n", licensesOutDir, err)
			}
		}
	}
	ConfigVars["{LICENSES_OUT_DIR}"] = *licensesOutDir

	// gnPath
	if *gnPath == "" && *outputLicenseFile {
		return fmt.Errorf("--gn_path cannot be empty.")
	}
	*gnPath = strings.ReplaceAll(*gnPath, "{FUCHSIA_DIR}", *fuchsiaDir)
	*gnPath, err = filepath.Abs(*gnPath)
	if err != nil {
		return fmt.Errorf("Failed to get absolute directory for *gnPath %v: %v", *gnPath, err)
	}

	*gnGenFile = strings.ReplaceAll(*gnGenFile, "{BUILD_DIR}", *buildDir)
	*gnGenFile, err = filepath.Abs(*gnGenFile)
	if err != nil {
		return fmt.Errorf("Failed to get absolute directory for *gnGenFile %v: %v", *gnGenFile, err)
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
	ConfigVars["{GN_GEN_FILE}"] = *gnPath
	ConfigVars["{OUTPUT_LICENSE_FILE}"] = strconv.FormatBool(*outputLicenseFile)
	ConfigVars["{RUN_ANALYSIS}"] = strconv.FormatBool(*runAnalysis)
	ConfigVars["{PRUNE_TARGETS}"] = string(pruneTargetsJSON)

	ConfigVars["{CHECK_URLS}"] = strconv.FormatBool(*checkURLs)

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
		return fmt.Errorf("failed to analyze the given directory: %v", err)
	}
	return nil
}

func main() {
	if err := mainImpl(); err != nil {
		fmt.Fprintf(os.Stderr, "check-licenses: %s\nSee go/fuchsia-licenses-playbook for information on resolving common errors.\n", err)
		os.Exit(1)
	}
}
