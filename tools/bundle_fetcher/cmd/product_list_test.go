// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/bundle_fetcher/bundler"
)

func TestProductListParseFlags(t *testing.T) {
	dir, err := os.MkdirTemp("", "bundle_fetcher_dir")
	if err != nil {
		t.Fatalf("unable to create temp dir")
	}
	tmpfn := filepath.Join(dir, "tmpfile")
	if err := os.WriteFile(tmpfn, []byte("hello world"), 0644); err != nil {
		t.Fatalf("unable to create temp file")
	}
	defer os.RemoveAll(dir)

	var tests = []struct {
		productListCmd *productListCmd
		expectedErr    string
	}{
		{
			productListCmd: &productListCmd{
				buildIDs:  "123456",
				gcsBucket: "orange",
				outDir:    dir,
			},
		},
		{
			productListCmd: &productListCmd{
				gcsBucket: "orange",
				outDir:    dir,
			},
			expectedErr: "-build_ids is required",
		},
		{
			productListCmd: &productListCmd{
				buildIDs: "123456",
				outDir:   dir,
			},
			expectedErr: "-bucket is required",
		},
		{
			productListCmd: &productListCmd{
				buildIDs:  "123456",
				gcsBucket: "orange",
			},
			expectedErr: "-out_dir is required",
		},
		{
			productListCmd: &productListCmd{
				buildIDs:  "123456",
				gcsBucket: "orange",
				outDir:    tmpfn,
			},
			expectedErr: fmt.Sprintf("out directory path %v is not a directory", tmpfn),
		},
	}

	for _, test := range tests {
		if err := test.productListCmd.parseFlags(); err != nil && err.Error() != test.expectedErr {
			t.Errorf("Got error: %s, want: %s", err.Error(), test.expectedErr)
		}
	}
}

func TestGetProductListFromJSON(t *testing.T) {
	contents := map[string][]byte{
		"some/valid/product_list.json": []byte(`[{
			"label": "//build/images/fuchsia:product_bundle(//build/toolchain/fuchsia:x64)",
			"name": "fake_product.x64",
			"path": "obj/build/images/fuchsia/product_bundle",
			"product_version": "fake_version",
			"transfer_manifest_path": "obj/build/images/fuchsia/transfer.json",
			"transfer_manifest_url": "file://obj/build/images/fuchsia/transfer.json"
		  }]`),
		"invalid/product_list.json": []byte(`[{
			"label": "//build/images/fuchsia:product_bundle(//build/toolchain/fuchsia:x64)",
			"path": "obj/build/images/fuchsia/product_bundle",
			"product_version": "fake_version"
		  }]`),
		"non-list/product_list.json": []byte(`{
			"label": "//build/images/fuchsia:product_bundle(//build/toolchain/fuchsia:x64)",
			"name": "fake_product.x64",
			"path": "obj/build/images/fuchsia/product_bundle",
			"product_version": "fake_version"
		  }`),
	}
	ctx := context.Background()
	var tests = []struct {
		name                string
		productListJSONPath string
		dataSinkErr         error
		expectedOutput      *build.ProductBundlesManifest
		expectedErrMessage  string
	}{
		{
			name:                "valid product_list.json",
			productListJSONPath: "some/valid/product_list.json",
			expectedOutput: &build.ProductBundlesManifest{
				build.ProductBundle{
					Label:                "//build/images/fuchsia:product_bundle(//build/toolchain/fuchsia:x64)",
					Name:                 "fake_product.x64",
					Path:                 "obj/build/images/fuchsia/product_bundle",
					ProductVersion:       "fake_version",
					TransferManifestPath: "obj/build/images/fuchsia/transfer.json",
					TransferManifestUrl:  "file://obj/build/images/fuchsia/transfer.json",
				},
			},
		},
		{
			name:                "not a list product_list.json",
			productListJSONPath: "non-list/product_list.json",
			expectedErrMessage:  "json: cannot unmarshal object into Go value of type []build.ProductBundle",
		},
		{
			name:                "missing something product_list.json",
			productListJSONPath: "invalid/product_list.json",
			expectedErrMessage:  "error, the product name is empty",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			sink := bundler.NewMemSink(contents, test.dataSinkErr, "")
			output, err := getProductListFromJSON(ctx, sink, test.productListJSONPath)
			if !reflect.DeepEqual(output, test.expectedOutput) {
				t.Errorf("Got output for '%s': %v, want: %v", test.name, output, test.expectedOutput)
			}
			if err != nil && err.Error() != test.expectedErrMessage {
				t.Errorf("Got error for '%s': %s, want: %s", test.name, err.Error(), test.expectedErrMessage)
			}
			if err == nil && test.expectedErrMessage != "" {
				t.Errorf("Got no error for '%s', want: %s", test.name, test.expectedErrMessage)
			}
		})
	}
}
