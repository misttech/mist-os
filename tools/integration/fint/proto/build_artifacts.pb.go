// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by protoc-gen-go. DO NOT EDIT.
// versions:
// 	protoc-gen-go v1.32.0
// 	protoc        v3.21.12
// source: build_artifacts.proto

package proto

import (
	protoreflect "google.golang.org/protobuf/reflect/protoreflect"
	protoimpl "google.golang.org/protobuf/runtime/protoimpl"
	structpb "google.golang.org/protobuf/types/known/structpb"
	reflect "reflect"
	sync "sync"
)

const (
	// Verify that this generated code is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(20 - protoimpl.MinVersion)
	// Verify that runtime/protoimpl is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(protoimpl.MaxVersion - 20)
)

// Various ninja build data.
type NinjaActionMetrics struct {
	state         protoimpl.MessageState
	sizeCache     protoimpl.SizeCache
	unknownFields protoimpl.UnknownFields

	// The number of actions to complete the build in the beginning.
	InitialActions int32 `protobuf:"varint,1,opt,name=initial_actions,json=initialActions,proto3" json:"initial_actions,omitempty"`
	// The number of actions executed at the end of the build.
	// This may be less than the number of initial actions due to `restat`
	// (dynamic action graph pruning).
	FinalActions int32 `protobuf:"varint,2,opt,name=final_actions,json=finalActions,proto3" json:"final_actions,omitempty"`
	// Breakdown of actions executed by type, e.g. "ACTION", "CXX", "STAMP".
	ActionsByType map[string]int32 `protobuf:"bytes,3,rep,name=actions_by_type,json=actionsByType,proto3" json:"actions_by_type,omitempty" protobuf_key:"bytes,1,opt,name=key,proto3" protobuf_val:"varint,2,opt,name=value,proto3"`
}

func (x *NinjaActionMetrics) Reset() {
	*x = NinjaActionMetrics{}
	if protoimpl.UnsafeEnabled {
		mi := &file_build_artifacts_proto_msgTypes[0]
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		ms.StoreMessageInfo(mi)
	}
}

func (x *NinjaActionMetrics) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*NinjaActionMetrics) ProtoMessage() {}

func (x *NinjaActionMetrics) ProtoReflect() protoreflect.Message {
	mi := &file_build_artifacts_proto_msgTypes[0]
	if protoimpl.UnsafeEnabled && x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use NinjaActionMetrics.ProtoReflect.Descriptor instead.
func (*NinjaActionMetrics) Descriptor() ([]byte, []int) {
	return file_build_artifacts_proto_rawDescGZIP(), []int{0}
}

func (x *NinjaActionMetrics) GetInitialActions() int32 {
	if x != nil {
		return x.InitialActions
	}
	return 0
}

func (x *NinjaActionMetrics) GetFinalActions() int32 {
	if x != nil {
		return x.FinalActions
	}
	return 0
}

func (x *NinjaActionMetrics) GetActionsByType() map[string]int32 {
	if x != nil {
		return x.ActionsByType
	}
	return nil
}

type DebugFile struct {
	state         protoimpl.MessageState
	sizeCache     protoimpl.SizeCache
	unknownFields protoimpl.UnknownFields

	// Absolute path to the file.
	Path string `protobuf:"bytes,1,opt,name=path,proto3" json:"path,omitempty"`
	// Relative path to which the file should be uploaded within the build's
	// namespace in cloud storage.
	UploadDest string `protobuf:"bytes,2,opt,name=upload_dest,json=uploadDest,proto3" json:"upload_dest,omitempty"`
}

func (x *DebugFile) Reset() {
	*x = DebugFile{}
	if protoimpl.UnsafeEnabled {
		mi := &file_build_artifacts_proto_msgTypes[1]
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		ms.StoreMessageInfo(mi)
	}
}

func (x *DebugFile) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*DebugFile) ProtoMessage() {}

func (x *DebugFile) ProtoReflect() protoreflect.Message {
	mi := &file_build_artifacts_proto_msgTypes[1]
	if protoimpl.UnsafeEnabled && x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use DebugFile.ProtoReflect.Descriptor instead.
func (*DebugFile) Descriptor() ([]byte, []int) {
	return file_build_artifacts_proto_rawDescGZIP(), []int{1}
}

func (x *DebugFile) GetPath() string {
	if x != nil {
		return x.Path
	}
	return ""
}

func (x *DebugFile) GetUploadDest() string {
	if x != nil {
		return x.UploadDest
	}
	return ""
}

// BuildArtifacts contains information about the targets built by `fint build`.
type BuildArtifacts struct {
	state         protoimpl.MessageState
	sizeCache     protoimpl.SizeCache
	unknownFields protoimpl.UnknownFields

	// A brief error log populated in case of a recognized failure mode (e.g. a
	// Ninja compilation failure).
	FailureSummary string `protobuf:"bytes,1,opt,name=failure_summary,json=failureSummary,proto3" json:"failure_summary,omitempty"`
	// Images produced by the build. We use a struct to avoid needing to maintain
	// a copy of the images.json schema here.
	BuiltImages []*structpb.Struct `protobuf:"bytes,2,rep,name=built_images,json=builtImages,proto3" json:"built_images,omitempty"`
	// Archives produced by the build. We use a struct to avoid needing to
	// maintain a copy of the images.json schema here.
	BuiltArchives []*structpb.Struct `protobuf:"bytes,3,rep,name=built_archives,json=builtArchives,proto3" json:"built_archives,omitempty"`
	// Collection of ninjatrace files to upload.
	NinjatraceJsonFiles []string `protobuf:"bytes,4,rep,name=ninjatrace_json_files,json=ninjatraceJsonFiles,proto3" json:"ninjatrace_json_files,omitempty"`
	// Collection of buildstats files to upload.
	BuildstatsJsonFiles []string `protobuf:"bytes,5,rep,name=buildstats_json_files,json=buildstatsJsonFiles,proto3" json:"buildstats_json_files,omitempty"`
	// The duration taken by the ninja build step.
	NinjaDurationSeconds int32 `protobuf:"varint,6,opt,name=ninja_duration_seconds,json=ninjaDurationSeconds,proto3" json:"ninja_duration_seconds,omitempty"`
	// Various ninja build data
	NinjaActionMetrics *NinjaActionMetrics `protobuf:"bytes,7,opt,name=ninja_action_metrics,json=ninjaActionMetrics,proto3" json:"ninja_action_metrics,omitempty"`
	// Mapping from user-friendly title to absolute path for important log files
	// that should be presented by the infrastructure for humans to read. We
	// reference the logs by path rather than inlining the contents in the
	// protobuf because the logs may be very long and inlining them would make it
	// very hard for humans to read the output proto.
	LogFiles map[string]string `protobuf:"bytes,8,rep,name=log_files,json=logFiles,proto3" json:"log_files,omitempty" protobuf_key:"bytes,1,opt,name=key,proto3" protobuf_val:"bytes,2,opt,name=value,proto3"`
	// Files to upload to cloud storage even if the build fails, to help with
	// debugging and understanding build performance. Files produced by ninja
	// targets themselves should *not* be uploaded via this mechanism, use
	// artifactory instead.
	DebugFiles []*DebugFile `protobuf:"bytes,9,rep,name=debug_files,json=debugFiles,proto3" json:"debug_files,omitempty"`
	// Whether an analysis of the build graph determined that the changed files do
	// not affect the build.
	BuildNotAffected bool `protobuf:"varint,10,opt,name=build_not_affected,json=buildNotAffected,proto3" json:"build_not_affected,omitempty"`
	// Names, as they appear in tests.json, of tests affected by the change under
	// tests. This is determined by doing a build graph analysis of the files
	// reported in the `changed_files` context spec field.
	AffectedTests []string `protobuf:"bytes,11,rep,name=affected_tests,json=affectedTests,proto3" json:"affected_tests,omitempty"`
}

func (x *BuildArtifacts) Reset() {
	*x = BuildArtifacts{}
	if protoimpl.UnsafeEnabled {
		mi := &file_build_artifacts_proto_msgTypes[2]
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		ms.StoreMessageInfo(mi)
	}
}

func (x *BuildArtifacts) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*BuildArtifacts) ProtoMessage() {}

func (x *BuildArtifacts) ProtoReflect() protoreflect.Message {
	mi := &file_build_artifacts_proto_msgTypes[2]
	if protoimpl.UnsafeEnabled && x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use BuildArtifacts.ProtoReflect.Descriptor instead.
func (*BuildArtifacts) Descriptor() ([]byte, []int) {
	return file_build_artifacts_proto_rawDescGZIP(), []int{2}
}

func (x *BuildArtifacts) GetFailureSummary() string {
	if x != nil {
		return x.FailureSummary
	}
	return ""
}

func (x *BuildArtifacts) GetBuiltImages() []*structpb.Struct {
	if x != nil {
		return x.BuiltImages
	}
	return nil
}

func (x *BuildArtifacts) GetBuiltArchives() []*structpb.Struct {
	if x != nil {
		return x.BuiltArchives
	}
	return nil
}

func (x *BuildArtifacts) GetNinjatraceJsonFiles() []string {
	if x != nil {
		return x.NinjatraceJsonFiles
	}
	return nil
}

func (x *BuildArtifacts) GetBuildstatsJsonFiles() []string {
	if x != nil {
		return x.BuildstatsJsonFiles
	}
	return nil
}

func (x *BuildArtifacts) GetNinjaDurationSeconds() int32 {
	if x != nil {
		return x.NinjaDurationSeconds
	}
	return 0
}

func (x *BuildArtifacts) GetNinjaActionMetrics() *NinjaActionMetrics {
	if x != nil {
		return x.NinjaActionMetrics
	}
	return nil
}

func (x *BuildArtifacts) GetLogFiles() map[string]string {
	if x != nil {
		return x.LogFiles
	}
	return nil
}

func (x *BuildArtifacts) GetDebugFiles() []*DebugFile {
	if x != nil {
		return x.DebugFiles
	}
	return nil
}

func (x *BuildArtifacts) GetBuildNotAffected() bool {
	if x != nil {
		return x.BuildNotAffected
	}
	return false
}

func (x *BuildArtifacts) GetAffectedTests() []string {
	if x != nil {
		return x.AffectedTests
	}
	return nil
}

var File_build_artifacts_proto protoreflect.FileDescriptor

var file_build_artifacts_proto_rawDesc = []byte{
	0x0a, 0x15, 0x62, 0x75, 0x69, 0x6c, 0x64, 0x5f, 0x61, 0x72, 0x74, 0x69, 0x66, 0x61, 0x63, 0x74,
	0x73, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x12, 0x04, 0x66, 0x69, 0x6e, 0x74, 0x1a, 0x1c, 0x67,
	0x6f, 0x6f, 0x67, 0x6c, 0x65, 0x2f, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x62, 0x75, 0x66, 0x2f, 0x73,
	0x74, 0x72, 0x75, 0x63, 0x74, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x22, 0xf9, 0x01, 0x0a, 0x12,
	0x4e, 0x69, 0x6e, 0x6a, 0x61, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x4d, 0x65, 0x74, 0x72, 0x69,
	0x63, 0x73, 0x12, 0x27, 0x0a, 0x0f, 0x69, 0x6e, 0x69, 0x74, 0x69, 0x61, 0x6c, 0x5f, 0x61, 0x63,
	0x74, 0x69, 0x6f, 0x6e, 0x73, 0x18, 0x01, 0x20, 0x01, 0x28, 0x05, 0x52, 0x0e, 0x69, 0x6e, 0x69,
	0x74, 0x69, 0x61, 0x6c, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x12, 0x23, 0x0a, 0x0d, 0x66,
	0x69, 0x6e, 0x61, 0x6c, 0x5f, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x18, 0x02, 0x20, 0x01,
	0x28, 0x05, 0x52, 0x0c, 0x66, 0x69, 0x6e, 0x61, 0x6c, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73,
	0x12, 0x53, 0x0a, 0x0f, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x5f, 0x62, 0x79, 0x5f, 0x74,
	0x79, 0x70, 0x65, 0x18, 0x03, 0x20, 0x03, 0x28, 0x0b, 0x32, 0x2b, 0x2e, 0x66, 0x69, 0x6e, 0x74,
	0x2e, 0x4e, 0x69, 0x6e, 0x6a, 0x61, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x4d, 0x65, 0x74, 0x72,
	0x69, 0x63, 0x73, 0x2e, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x42, 0x79, 0x54, 0x79, 0x70,
	0x65, 0x45, 0x6e, 0x74, 0x72, 0x79, 0x52, 0x0d, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x42,
	0x79, 0x54, 0x79, 0x70, 0x65, 0x1a, 0x40, 0x0a, 0x12, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x73,
	0x42, 0x79, 0x54, 0x79, 0x70, 0x65, 0x45, 0x6e, 0x74, 0x72, 0x79, 0x12, 0x10, 0x0a, 0x03, 0x6b,
	0x65, 0x79, 0x18, 0x01, 0x20, 0x01, 0x28, 0x09, 0x52, 0x03, 0x6b, 0x65, 0x79, 0x12, 0x14, 0x0a,
	0x05, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x18, 0x02, 0x20, 0x01, 0x28, 0x05, 0x52, 0x05, 0x76, 0x61,
	0x6c, 0x75, 0x65, 0x3a, 0x02, 0x38, 0x01, 0x22, 0x40, 0x0a, 0x09, 0x44, 0x65, 0x62, 0x75, 0x67,
	0x46, 0x69, 0x6c, 0x65, 0x12, 0x12, 0x0a, 0x04, 0x70, 0x61, 0x74, 0x68, 0x18, 0x01, 0x20, 0x01,
	0x28, 0x09, 0x52, 0x04, 0x70, 0x61, 0x74, 0x68, 0x12, 0x1f, 0x0a, 0x0b, 0x75, 0x70, 0x6c, 0x6f,
	0x61, 0x64, 0x5f, 0x64, 0x65, 0x73, 0x74, 0x18, 0x02, 0x20, 0x01, 0x28, 0x09, 0x52, 0x0a, 0x75,
	0x70, 0x6c, 0x6f, 0x61, 0x64, 0x44, 0x65, 0x73, 0x74, 0x22, 0xa4, 0x05, 0x0a, 0x0e, 0x42, 0x75,
	0x69, 0x6c, 0x64, 0x41, 0x72, 0x74, 0x69, 0x66, 0x61, 0x63, 0x74, 0x73, 0x12, 0x27, 0x0a, 0x0f,
	0x66, 0x61, 0x69, 0x6c, 0x75, 0x72, 0x65, 0x5f, 0x73, 0x75, 0x6d, 0x6d, 0x61, 0x72, 0x79, 0x18,
	0x01, 0x20, 0x01, 0x28, 0x09, 0x52, 0x0e, 0x66, 0x61, 0x69, 0x6c, 0x75, 0x72, 0x65, 0x53, 0x75,
	0x6d, 0x6d, 0x61, 0x72, 0x79, 0x12, 0x3a, 0x0a, 0x0c, 0x62, 0x75, 0x69, 0x6c, 0x74, 0x5f, 0x69,
	0x6d, 0x61, 0x67, 0x65, 0x73, 0x18, 0x02, 0x20, 0x03, 0x28, 0x0b, 0x32, 0x17, 0x2e, 0x67, 0x6f,
	0x6f, 0x67, 0x6c, 0x65, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x62, 0x75, 0x66, 0x2e, 0x53, 0x74,
	0x72, 0x75, 0x63, 0x74, 0x52, 0x0b, 0x62, 0x75, 0x69, 0x6c, 0x74, 0x49, 0x6d, 0x61, 0x67, 0x65,
	0x73, 0x12, 0x3e, 0x0a, 0x0e, 0x62, 0x75, 0x69, 0x6c, 0x74, 0x5f, 0x61, 0x72, 0x63, 0x68, 0x69,
	0x76, 0x65, 0x73, 0x18, 0x03, 0x20, 0x03, 0x28, 0x0b, 0x32, 0x17, 0x2e, 0x67, 0x6f, 0x6f, 0x67,
	0x6c, 0x65, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x62, 0x75, 0x66, 0x2e, 0x53, 0x74, 0x72, 0x75,
	0x63, 0x74, 0x52, 0x0d, 0x62, 0x75, 0x69, 0x6c, 0x74, 0x41, 0x72, 0x63, 0x68, 0x69, 0x76, 0x65,
	0x73, 0x12, 0x32, 0x0a, 0x15, 0x6e, 0x69, 0x6e, 0x6a, 0x61, 0x74, 0x72, 0x61, 0x63, 0x65, 0x5f,
	0x6a, 0x73, 0x6f, 0x6e, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x73, 0x18, 0x04, 0x20, 0x03, 0x28, 0x09,
	0x52, 0x13, 0x6e, 0x69, 0x6e, 0x6a, 0x61, 0x74, 0x72, 0x61, 0x63, 0x65, 0x4a, 0x73, 0x6f, 0x6e,
	0x46, 0x69, 0x6c, 0x65, 0x73, 0x12, 0x32, 0x0a, 0x15, 0x62, 0x75, 0x69, 0x6c, 0x64, 0x73, 0x74,
	0x61, 0x74, 0x73, 0x5f, 0x6a, 0x73, 0x6f, 0x6e, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x73, 0x18, 0x05,
	0x20, 0x03, 0x28, 0x09, 0x52, 0x13, 0x62, 0x75, 0x69, 0x6c, 0x64, 0x73, 0x74, 0x61, 0x74, 0x73,
	0x4a, 0x73, 0x6f, 0x6e, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x12, 0x34, 0x0a, 0x16, 0x6e, 0x69, 0x6e,
	0x6a, 0x61, 0x5f, 0x64, 0x75, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x5f, 0x73, 0x65, 0x63, 0x6f,
	0x6e, 0x64, 0x73, 0x18, 0x06, 0x20, 0x01, 0x28, 0x05, 0x52, 0x14, 0x6e, 0x69, 0x6e, 0x6a, 0x61,
	0x44, 0x75, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x53, 0x65, 0x63, 0x6f, 0x6e, 0x64, 0x73, 0x12,
	0x4a, 0x0a, 0x14, 0x6e, 0x69, 0x6e, 0x6a, 0x61, 0x5f, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x5f,
	0x6d, 0x65, 0x74, 0x72, 0x69, 0x63, 0x73, 0x18, 0x07, 0x20, 0x01, 0x28, 0x0b, 0x32, 0x18, 0x2e,
	0x66, 0x69, 0x6e, 0x74, 0x2e, 0x4e, 0x69, 0x6e, 0x6a, 0x61, 0x41, 0x63, 0x74, 0x69, 0x6f, 0x6e,
	0x4d, 0x65, 0x74, 0x72, 0x69, 0x63, 0x73, 0x52, 0x12, 0x6e, 0x69, 0x6e, 0x6a, 0x61, 0x41, 0x63,
	0x74, 0x69, 0x6f, 0x6e, 0x4d, 0x65, 0x74, 0x72, 0x69, 0x63, 0x73, 0x12, 0x3f, 0x0a, 0x09, 0x6c,
	0x6f, 0x67, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x73, 0x18, 0x08, 0x20, 0x03, 0x28, 0x0b, 0x32, 0x22,
	0x2e, 0x66, 0x69, 0x6e, 0x74, 0x2e, 0x42, 0x75, 0x69, 0x6c, 0x64, 0x41, 0x72, 0x74, 0x69, 0x66,
	0x61, 0x63, 0x74, 0x73, 0x2e, 0x4c, 0x6f, 0x67, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x45, 0x6e, 0x74,
	0x72, 0x79, 0x52, 0x08, 0x6c, 0x6f, 0x67, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x12, 0x30, 0x0a, 0x0b,
	0x64, 0x65, 0x62, 0x75, 0x67, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x73, 0x18, 0x09, 0x20, 0x03, 0x28,
	0x0b, 0x32, 0x0f, 0x2e, 0x66, 0x69, 0x6e, 0x74, 0x2e, 0x44, 0x65, 0x62, 0x75, 0x67, 0x46, 0x69,
	0x6c, 0x65, 0x52, 0x0a, 0x64, 0x65, 0x62, 0x75, 0x67, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x12, 0x2c,
	0x0a, 0x12, 0x62, 0x75, 0x69, 0x6c, 0x64, 0x5f, 0x6e, 0x6f, 0x74, 0x5f, 0x61, 0x66, 0x66, 0x65,
	0x63, 0x74, 0x65, 0x64, 0x18, 0x0a, 0x20, 0x01, 0x28, 0x08, 0x52, 0x10, 0x62, 0x75, 0x69, 0x6c,
	0x64, 0x4e, 0x6f, 0x74, 0x41, 0x66, 0x66, 0x65, 0x63, 0x74, 0x65, 0x64, 0x12, 0x25, 0x0a, 0x0e,
	0x61, 0x66, 0x66, 0x65, 0x63, 0x74, 0x65, 0x64, 0x5f, 0x74, 0x65, 0x73, 0x74, 0x73, 0x18, 0x0b,
	0x20, 0x03, 0x28, 0x09, 0x52, 0x0d, 0x61, 0x66, 0x66, 0x65, 0x63, 0x74, 0x65, 0x64, 0x54, 0x65,
	0x73, 0x74, 0x73, 0x1a, 0x3b, 0x0a, 0x0d, 0x4c, 0x6f, 0x67, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x45,
	0x6e, 0x74, 0x72, 0x79, 0x12, 0x10, 0x0a, 0x03, 0x6b, 0x65, 0x79, 0x18, 0x01, 0x20, 0x01, 0x28,
	0x09, 0x52, 0x03, 0x6b, 0x65, 0x79, 0x12, 0x14, 0x0a, 0x05, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x18,
	0x02, 0x20, 0x01, 0x28, 0x09, 0x52, 0x05, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x3a, 0x02, 0x38, 0x01,
	0x42, 0x35, 0x5a, 0x33, 0x67, 0x6f, 0x2e, 0x66, 0x75, 0x63, 0x68, 0x73, 0x69, 0x61, 0x2e, 0x64,
	0x65, 0x76, 0x2f, 0x66, 0x75, 0x63, 0x68, 0x73, 0x69, 0x61, 0x2f, 0x74, 0x6f, 0x6f, 0x6c, 0x73,
	0x2f, 0x69, 0x6e, 0x74, 0x65, 0x67, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x2f, 0x66, 0x69, 0x6e,
	0x74, 0x2f, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x62, 0x06, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x33,
}

var (
	file_build_artifacts_proto_rawDescOnce sync.Once
	file_build_artifacts_proto_rawDescData = file_build_artifacts_proto_rawDesc
)

func file_build_artifacts_proto_rawDescGZIP() []byte {
	file_build_artifacts_proto_rawDescOnce.Do(func() {
		file_build_artifacts_proto_rawDescData = protoimpl.X.CompressGZIP(file_build_artifacts_proto_rawDescData)
	})
	return file_build_artifacts_proto_rawDescData
}

var file_build_artifacts_proto_msgTypes = make([]protoimpl.MessageInfo, 5)
var file_build_artifacts_proto_goTypes = []interface{}{
	(*NinjaActionMetrics)(nil), // 0: fint.NinjaActionMetrics
	(*DebugFile)(nil),          // 1: fint.DebugFile
	(*BuildArtifacts)(nil),     // 2: fint.BuildArtifacts
	nil,                        // 3: fint.NinjaActionMetrics.ActionsByTypeEntry
	nil,                        // 4: fint.BuildArtifacts.LogFilesEntry
	(*structpb.Struct)(nil),    // 5: google.protobuf.Struct
}
var file_build_artifacts_proto_depIdxs = []int32{
	3, // 0: fint.NinjaActionMetrics.actions_by_type:type_name -> fint.NinjaActionMetrics.ActionsByTypeEntry
	5, // 1: fint.BuildArtifacts.built_images:type_name -> google.protobuf.Struct
	5, // 2: fint.BuildArtifacts.built_archives:type_name -> google.protobuf.Struct
	0, // 3: fint.BuildArtifacts.ninja_action_metrics:type_name -> fint.NinjaActionMetrics
	4, // 4: fint.BuildArtifacts.log_files:type_name -> fint.BuildArtifacts.LogFilesEntry
	1, // 5: fint.BuildArtifacts.debug_files:type_name -> fint.DebugFile
	6, // [6:6] is the sub-list for method output_type
	6, // [6:6] is the sub-list for method input_type
	6, // [6:6] is the sub-list for extension type_name
	6, // [6:6] is the sub-list for extension extendee
	0, // [0:6] is the sub-list for field type_name
}

func init() { file_build_artifacts_proto_init() }
func file_build_artifacts_proto_init() {
	if File_build_artifacts_proto != nil {
		return
	}
	if !protoimpl.UnsafeEnabled {
		file_build_artifacts_proto_msgTypes[0].Exporter = func(v interface{}, i int) interface{} {
			switch v := v.(*NinjaActionMetrics); i {
			case 0:
				return &v.state
			case 1:
				return &v.sizeCache
			case 2:
				return &v.unknownFields
			default:
				return nil
			}
		}
		file_build_artifacts_proto_msgTypes[1].Exporter = func(v interface{}, i int) interface{} {
			switch v := v.(*DebugFile); i {
			case 0:
				return &v.state
			case 1:
				return &v.sizeCache
			case 2:
				return &v.unknownFields
			default:
				return nil
			}
		}
		file_build_artifacts_proto_msgTypes[2].Exporter = func(v interface{}, i int) interface{} {
			switch v := v.(*BuildArtifacts); i {
			case 0:
				return &v.state
			case 1:
				return &v.sizeCache
			case 2:
				return &v.unknownFields
			default:
				return nil
			}
		}
	}
	type x struct{}
	out := protoimpl.TypeBuilder{
		File: protoimpl.DescBuilder{
			GoPackagePath: reflect.TypeOf(x{}).PkgPath(),
			RawDescriptor: file_build_artifacts_proto_rawDesc,
			NumEnums:      0,
			NumMessages:   5,
			NumExtensions: 0,
			NumServices:   0,
		},
		GoTypes:           file_build_artifacts_proto_goTypes,
		DependencyIndexes: file_build_artifacts_proto_depIdxs,
		MessageInfos:      file_build_artifacts_proto_msgTypes,
	}.Build()
	File_build_artifacts_proto = out.File
	file_build_artifacts_proto_rawDesc = nil
	file_build_artifacts_proto_goTypes = nil
	file_build_artifacts_proto_depIdxs = nil
}
