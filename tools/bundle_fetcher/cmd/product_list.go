// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/bundle_fetcher/bundler"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"

	"github.com/google/subcommands"
)

// A portion of the JSON file, the pieces needed below.
type productBundleMetadataV2 struct {
	Version     string `json:"version"`
	ProductName string `json:"product_name"`
}

type productListCmd struct {
	gcsBucket                 string
	buildIDs                  string
	outDir                    string
	outputProductListFileName string
}

const (
	buildsDirName          = "builds"
	buildApiDirName        = "build_api"
	productBundlesJSONName = "product_bundles.json"
	transferJSONName       = "transfer.json"
)

func (*productListCmd) Name() string { return "product-list" }

func (*productListCmd) Synopsis() string {
	return "Create a product list to index urls to transfer manifest files."
}

func (*productListCmd) Usage() string {
	return "bundle_fetcher product-list -bucket <GCS_BUCKET> -build_ids <build_ids>\n"
}

func (cmd *productListCmd) SetFlags(f *flag.FlagSet) {
	f.StringVar(&cmd.gcsBucket, "bucket", "", "GCS bucket from which to read the files from.")
	f.StringVar(&cmd.buildIDs, "build_ids", "", "Comma separated list of build_ids.")
	f.StringVar(&cmd.outDir, "out_dir", "", "Directory to write out_file_name to.")
	f.StringVar(&cmd.outputProductListFileName, "out_file_name", "product_bundles.json", "Name of the output file containing the product bundle to transfer lookup information.")
}

func (cmd *productListCmd) parseFlags() error {
	if cmd.buildIDs == "" {
		return fmt.Errorf("-build_ids is required")
	}

	if cmd.gcsBucket == "" {
		return fmt.Errorf("-bucket is required")
	}

	if cmd.outDir == "" {
		return fmt.Errorf("-out_dir is required")
	}
	info, err := os.Stat(cmd.outDir)
	if os.IsNotExist(err) {
		return fmt.Errorf("out directory path %s does not exist", cmd.outDir)
	}
	if err != nil {
		return err
	}
	if !info.IsDir() {
		return fmt.Errorf("out directory path %s is not a directory", cmd.outDir)
	}
	return nil
}

func (cmd *productListCmd) Execute(ctx context.Context, f *flag.FlagSet, _ ...interface{}) subcommands.ExitStatus {
	if err := cmd.execute(ctx); err != nil {
		logger.Errorf(ctx, "%s", err)
		return subcommands.ExitFailure
	}
	return subcommands.ExitSuccess
}

func (cmd *productListCmd) execute(ctx context.Context) error {
	if err := cmd.parseFlags(); err != nil {
		return err
	}

	sink, err := bundler.NewCloudSink(ctx, cmd.gcsBucket)
	if err != nil {
		return err
	}
	defer sink.Close()

	return cmd.executeWithSink(ctx, sink)
}

func (cmd *productListCmd) executeWithSink(ctx context.Context, sink bundler.DataSink) error {
	productList := build.ProductBundlesManifest{}

	buildIDsList := strings.Split(cmd.buildIDs, ",")
	for _, buildID := range buildIDsList {
		buildID = strings.TrimSpace(buildID)
		productBundlePath := filepath.Join(buildsDirName, buildID, buildApiDirName, productBundlesJSONName)
		logger.Debugf(ctx, "Build %s contains the product bundles in path %s", buildID, productBundlePath)
		productBundles, err := getProductListFromJSON(ctx, sink, productBundlePath)
		if err != nil {
			return fmt.Errorf("unable to read product bundle metdadata for build_id %s: %s %w", buildID, productBundlePath, err)
		}
		for _, productBundle := range *productBundles {
			productEntry := build.ProductBundle{
				// Entries for Label and TransferManifestPath are not needed.
				Name:                productBundle.Name,
				ProductVersion:      productBundle.ProductVersion,
				TransferManifestUrl: fmt.Sprintf("gs://%s/%s/%s/%s", cmd.gcsBucket, buildsDirName, buildID, transferJSONName),
			}
			productList = append(productList, productEntry)
		}
	}
	outputFilePath := filepath.Join(cmd.outDir, cmd.outputProductListFileName)
	logger.Debugf(ctx, "writing final product list file to: %s", outputFilePath)
	f, err := os.Create(outputFilePath)
	if err != nil {
		return fmt.Errorf("Creating '%s' %w", outputFilePath, err)
	}
	var errs error
	enc := json.NewEncoder(f)
	enc.SetIndent("", "  ")
	if err := enc.Encode(&productList); err != nil {
		errs = errors.Join(errs, err)
	}
	if err := f.Close(); err != nil {
		errs = errors.Join(errs, err)
	}
	return errs
}

// Readh the product_bundles.json from GCS and returns them as a
// ProductBundlesManifest list.
func getProductListFromJSON(ctx context.Context, sink bundler.DataSink, productListJSONPath string) (*build.ProductBundlesManifest, error) {
	data, err := sink.ReadFromGCS(ctx, productListJSONPath)
	if err != nil {
		return nil, err
	}
	listData := &build.ProductBundlesManifest{}
	err = json.Unmarshal(data, listData)
	if err != nil {
		return nil, err
	}
	if len(*listData) == 0 {
		return nil, fmt.Errorf("error, the product list is empty")
	}
	for _, product := range *listData {
		// Only needed values (by this tool) are validated.
		if product.Name == "" {
			return nil, fmt.Errorf("error, the product name is empty")
		}
		if product.ProductVersion == "" {
			return nil, fmt.Errorf("error, the product version is empty")
		}
	}
	return listData, nil
}
