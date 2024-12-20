// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_H_

#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>

#include <map>
#include <memory>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "src/ui/lib/escher/flib/fence_queue.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/flatland/flatland_presenter.h"
#include "src/ui/scenic/lib/flatland/link_system.h"
#include "src/ui/scenic/lib/flatland/transform_graph.h"
#include "src/ui/scenic/lib/flatland/transform_handle.h"
#include "src/ui/scenic/lib/flatland/uber_struct_system.h"
#include "src/ui/scenic/lib/scenic/util/error_reporter.h"
#include "src/ui/scenic/lib/scheduling/id.h"
#include "src/ui/scenic/lib/scheduling/present2_helper.h"
#include "src/ui/scenic/lib/utils/dispatcher_holder.h"

#include <glm/glm.hpp>
#include <glm/mat3x3.hpp>
#include <glm/vec2.hpp>

namespace flatland {

// Implements the `fuchsia.ui.composition.Flatland` protocol.  It is intended to run on its own
// thread/dispatcher, and communicates with the main/render thread(s) via the UberStruct mechanism,
// as well as other interfaces such as FlatlandPresenter.  Because `fuchsia.ui.composition.Flatland`
// is a stateful protocol, each client is connected to a different Flatland object.
class Flatland : public fidl::Server<fuchsia_ui_composition::Flatland>,
                 public std::enable_shared_from_this<Flatland> {
 public:
  using BufferCollectionId = uint64_t;
  using ContentId = fuchsia_ui_composition::ContentId;
  using FuturePresentationInfos = std::vector<fuchsia_scenic_scheduling::PresentationInfo>;
  using TransformId = fuchsia_ui_composition::TransformId;

  // Instantiates a new Flatland object and binds it to serve the Flatland protocol over the
  // `server_end` channel.  Method invocations received on this channel will be serviced on
  // the thread managed by `dispatcher_holder`.
  //
  // The `destroy_instance_function` is called to notify the instance's manager that the instance
  // should be destroyed.  This function is invoked on the thread owned by `dispatcher_holder`. When
  // this function is invoked, the client FIDL connection has already been closed.  There are two
  // situations that result in the invocation of `destroy_instance_function`:
  //   - the client closes the FIDL channel
  //   - the client makes illegal use of the API (or associated APIs like ChildViewWatcher)
  //
  // `flatland_presenter`, `link_system`, `uber_struct_queue`, and `buffer_collection_importers`
  // allow this Flatland object to access resources shared by all Flatland instances for actions
  // like frame scheduling, linking, buffer allocation, and presentation to the global scene graph.
  static std::shared_ptr<Flatland> New(
      std::shared_ptr<utils::DispatcherHolder> dispatcher_holder,
      fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end,
      scheduling::SessionId session_id, std::function<void()> destroy_instance_function,
      std::shared_ptr<FlatlandPresenter> flatland_presenter,
      std::shared_ptr<LinkSystem> link_system,
      std::shared_ptr<UberStructSystem::UberStructQueue> uber_struct_queue,
      const std::vector<std::shared_ptr<allocation::BufferCollectionImporter>>&
          buffer_collection_importers,
      fit::function<void(fidl::ServerEnd<fuchsia_ui_views::Focuser>, zx_koid_t)>
          register_view_focuser,
      fit::function<void(fidl::ServerEnd<fuchsia_ui_views::ViewRefFocused>, zx_koid_t)>
          register_view_ref_focused,
      fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::TouchSource>, zx_koid_t)>
          register_touch_source,
      fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::MouseSource>, zx_koid_t)>
          register_mouse_source);

  // Because this object captures its "this" pointer in internal closures, it is unsafe to copy or
  // move it. Disable all copy and move operations.
  Flatland(const Flatland&) = delete;
  Flatland& operator=(const Flatland&) = delete;
  Flatland(Flatland&&) = delete;
  Flatland& operator=(Flatland&&) = delete;

  ~Flatland() override;

  // |fuchsia_ui_composition::Flatland|
  void Present(PresentRequest& request, PresentCompleter::Sync& completer) override;
  void Present(fuchsia_ui_composition::PresentArgs args);

  // |fuchsia_ui_composition::Flatland|
  void CreateView(CreateViewRequest& request, CreateViewCompleter::Sync& completer) override;
  void CreateView(
      fuchsia_ui_views::ViewCreationToken token,
      fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher);
  void CreateView2(CreateView2Request& request, CreateView2Completer::Sync& completer) override;
  void CreateView2(
      fuchsia_ui_views::ViewCreationToken token,
      fuchsia_ui_views::ViewIdentityOnCreation view_identity,
      fuchsia_ui_composition::ViewBoundProtocols protocols,
      fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher);

  // |fuchsia_ui_composition::Flatland|
  // TODO(https://fxbug.dev/42162046): Consider returning tokens for re-linking.
  void ReleaseView(ReleaseViewCompleter::Sync& completer) override;
  void ReleaseView();

  // |fuchsia_ui_composition::Flatland|
  void Clear(ClearCompleter::Sync& completer) override;
  void Clear();

  // |fuchsia_ui_composition::Flatland|
  void CreateTransform(CreateTransformRequest& request,
                       CreateTransformCompleter::Sync& completer) override;
  void CreateTransform(TransformId transform_id);

  // |fuchsia_ui_composition::Flatland|
  void SetTranslation(SetTranslationRequest& request,
                      SetTranslationCompleter::Sync& completer) override;
  void SetTranslation(TransformId transform_id, fuchsia_math::Vec translation);

  // |fuchsia_ui_composition::Flatland|
  void SetOrientation(SetOrientationRequest& request,
                      SetOrientationCompleter::Sync& completer) override;
  void SetOrientation(TransformId transform_id, fuchsia_ui_composition::Orientation orientation);

  // |fuchsia_ui_composition::Flatland|
  void SetScale(SetScaleRequest& request, SetScaleCompleter::Sync& completer) override;
  void SetScale(TransformId transform_id, fuchsia_math::VecF scale);

  // |fuchsia_ui_composition::Flatland|
  void SetOpacity(SetOpacityRequest& request, SetOpacityCompleter::Sync& completer) override;
  void SetOpacity(TransformId transform_id, float value);

  // |fuchsia_ui_composition::Flatland|
  void SetClipBoundary(SetClipBoundaryRequest& request,
                       SetClipBoundaryCompleter::Sync& completer) override;
  void SetClipBoundary(TransformId transform_id, fidl::Box<fuchsia_math::Rect> bounds);

  // |fuchsia_ui_composition::Flatland|
  void AddChild(AddChildRequest& request, AddChildCompleter::Sync& completer) override;
  void AddChild(TransformId parent_transform_id, TransformId child_transform_id);

  // |fuchsia_ui_composition::Flatland|
  void RemoveChild(RemoveChildRequest& request, RemoveChildCompleter::Sync& completer) override;
  void RemoveChild(TransformId parent_transform_id, TransformId child_transform_id);

  // |fuchsia_ui_composition::Flatland|
  void ReplaceChildren(ReplaceChildrenRequest& request,
                       ReplaceChildrenCompleter::Sync& completer) override;
  void ReplaceChildren(TransformId parent_transform_id,
                       const std::vector<TransformId>& new_child_transform_ids);

  // |fuchsia_ui_composition::Flatland|
  void SetRootTransform(SetRootTransformRequest& request,
                        SetRootTransformCompleter::Sync& completer) override;
  void SetRootTransform(TransformId transform_id);

  // |fuchsia_ui_composition::Flatland|
  void CreateViewport(CreateViewportRequest& request,
                      CreateViewportCompleter::Sync& completer) override;
  void CreateViewport(ContentId viewport_id, fuchsia_ui_views::ViewportCreationToken token,
                      fuchsia_ui_composition::ViewportProperties properties,
                      fidl::ServerEnd<fuchsia_ui_composition::ChildViewWatcher> child_view_watcher);

  // |fuchsia_ui_composition::Flatland|
  void CreateImage(CreateImageRequest& request, CreateImageCompleter::Sync& completer) override;
  void CreateImage(ContentId image_id,
                   fuchsia_ui_composition::BufferCollectionImportToken import_token,
                   uint32_t vmo_index, fuchsia_ui_composition::ImageProperties properties);

  // |fuchsia_ui_composition::Flatland|
  void SetImageSampleRegion(SetImageSampleRegionRequest& request,
                            SetImageSampleRegionCompleter::Sync& completer) override;
  void SetImageSampleRegion(ContentId image_id, fuchsia_math::RectF rect);

  // |fuchsia_ui_composition::Flatland|
  void SetImageDestinationSize(SetImageDestinationSizeRequest& request,
                               SetImageDestinationSizeCompleter::Sync& completer) override;
  void SetImageDestinationSize(ContentId image_id, fuchsia_math::SizeU size);

  // |fuchsia_ui_composition::Flatland|
  void SetImageBlendingFunction(SetImageBlendingFunctionRequest& request,
                                SetImageBlendingFunctionCompleter::Sync& completer) override;
  void SetImageBlendingFunction(ContentId image_id, fuchsia_ui_composition::BlendMode blend_mode);

  // |fuchsia_ui_composition::Flatland|
  void SetImageFlip(SetImageFlipRequest& request, SetImageFlipCompleter::Sync& completer) override;
  void SetImageFlip(ContentId image_id, fuchsia_ui_composition::ImageFlip flip);

  // |fuchsia_ui_composition::Flatland|
  void CreateFilledRect(CreateFilledRectRequest& request,
                        CreateFilledRectCompleter::Sync& completer) override;
  void CreateFilledRect(ContentId rect_id);

  // |fuchsia_ui_composition::Flatland|
  void SetSolidFill(SetSolidFillRequest& request, SetSolidFillCompleter::Sync& completer) override;
  void SetSolidFill(ContentId rect_id, fuchsia_ui_composition::ColorRgba color,
                    fuchsia_math::SizeU size);

  // |fuchsia_ui_composition::Flatland|
  void ReleaseFilledRect(ReleaseFilledRectRequest& request,
                         ReleaseFilledRectCompleter::Sync& completer) override;
  void ReleaseFilledRect(ContentId rect_id);

  // |fuchsia_ui_composition::Flatland|
  void SetImageOpacity(SetImageOpacityRequest& request,
                       SetImageOpacityCompleter::Sync& completer) override;
  void SetImageOpacity(ContentId image_id, float opacity);

  // |fuchsia_ui_composition::Flatland|
  void SetHitRegions(SetHitRegionsRequest& request,
                     SetHitRegionsCompleter::Sync& completer) override;
  void SetHitRegions(TransformId transform_id,
                     std::vector<fuchsia_ui_composition::HitRegion> regions);

  // |fuchsia_ui_composition::Flatland|
  void SetInfiniteHitRegion(SetInfiniteHitRegionRequest& request,
                            SetInfiniteHitRegionCompleter::Sync& completer) override;
  void SetInfiniteHitRegion(TransformId transform_id,
                            fuchsia_ui_composition::HitTestInteraction hit_test);

  // |fuchsia_ui_composition::Flatland|
  void SetContent(SetContentRequest& request, SetContentCompleter::Sync& completer) override;
  void SetContent(TransformId transform_id, ContentId content_id);

  // |fuchsia_ui_composition::Flatland|
  void SetViewportProperties(SetViewportPropertiesRequest& request,
                             SetViewportPropertiesCompleter::Sync& completer) override;
  void SetViewportProperties(ContentId viewport_id,
                             fuchsia_ui_composition::ViewportProperties properties);

  // |fuchsia_ui_composition::Flatland|
  void ReleaseTransform(ReleaseTransformRequest& request,
                        ReleaseTransformCompleter::Sync& completer) override;
  void ReleaseTransform(TransformId transform_id);

  // |fuchsia_ui_composition::Flatland|
  void ReleaseViewport(ReleaseViewportRequest& request,
                       ReleaseViewportCompleter::Sync& completer) override;
  void ReleaseViewport(ContentId viewport_id,
                       fit::function<void(fuchsia_ui_views::ViewportCreationToken)> completer);

  // |fuchsia_ui_composition::Flatland|
  void ReleaseImage(ReleaseImageRequest& request, ReleaseImageCompleter::Sync& completer) override;
  void ReleaseImage(ContentId image_id);

  // |fuchsia_ui_composition::Flatland|
  void SetDebugName(SetDebugNameRequest& request, SetDebugNameCompleter::Sync& completer) override;
  void SetDebugName(std::string name);

  // Called just before the FIDL client receives the event of the same name, indicating that this
  // Flatland instance should allow a |additional_present_credits| calls to Present().
  void OnNextFrameBegin(uint32_t additional_present_credits,
                        FuturePresentationInfos presentation_infos);

  // Called when this Flatland instance should send the OnFramePresented() event to the FIDL
  // client.
  void OnFramePresented(const std::map<scheduling::PresentId, zx::time>& latched_times,
                        scheduling::PresentTimestamps present_times);

  // For validating the transform hierarchy in tests only. For the sake of testing, the "root" will
  // always be the top-most TransformHandle from the TransformGraph owned by this Flatland. If
  // currently linked to a parent, that means the Link's child_transform_handle. If not, that means
  // the local_root_.
  TransformHandle GetRoot() const;

  // For validating properties associated with content in tests only. If |content_id| does not
  // exist for this Flatland instance, returns std::nullopt.
  std::optional<TransformHandle> GetContentHandle(ContentId content_id) const;

  // For validating properties associated with transforms in tests only. If |transform_id| does not
  // exist for this Flatland instance, returns std::nullopt.
  std::optional<TransformHandle> GetTransformHandle(TransformId transform_id) const;

  // For validating logs in tests only.
  void SetErrorReporter(std::unique_ptr<scenic_impl::ErrorReporter> error_reporter);

  // For using as a unique identifier in tests only.
  scheduling::SessionId GetSessionId() const;

 private:
  Flatland(std::shared_ptr<utils::DispatcherHolder> dispatcher_holder,
           scheduling::SessionId session_id, std::shared_ptr<FlatlandPresenter> flatland_presenter,
           std::shared_ptr<LinkSystem> link_system,
           std::shared_ptr<UberStructSystem::UberStructQueue> uber_struct_queue,
           const std::vector<std::shared_ptr<allocation::BufferCollectionImporter>>&
               buffer_collection_importers,
           fit::function<void(fidl::ServerEnd<fuchsia_ui_views::Focuser>, zx_koid_t)>
               register_view_focuser,
           fit::function<void(fidl::ServerEnd<fuchsia_ui_views::ViewRefFocused>, zx_koid_t)>
               register_view_ref_focused,
           fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::TouchSource>, zx_koid_t)>
               register_touch_source,
           fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::MouseSource>, zx_koid_t)>
               register_mouse_source);

  // `Flatland::New()` dispatches a task to invoke this.
  void Bind(fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end,
            std::function<void()> destroy_instance_function);

  void OnFidlClosed(fidl::UnbindInfo unbind_info);

  void ReportLinkProtocolError(const std::string& error_log);
  void CloseConnection(fuchsia_ui_composition::FlatlandError error);

  // Note: Any new CreateView function must use this helper function for it to have the same
  // test coverage as its siblings.
  void CreateViewHelper(
      fuchsia_ui_views::ViewCreationToken token,
      fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher,
      std::optional<fuchsia_ui_views::ViewIdentityOnCreation> view_identity,
      std::optional<fuchsia_ui_composition::ViewBoundProtocols> protocols);

  void RegisterViewBoundProtocols(fuchsia_ui_composition::ViewBoundProtocols protocols,
                                  zx_koid_t view_ref_koid);

  // Sets clip bounds on the provided transform handle. Takes in TransformHandle and not
  // TransformID as a parameter so that it can be applied to content transforms that do
  // not have an external ID that they are mapped to.
  void SetClipBoundaryInternal(TransformHandle handle, fuchsia_math::Rect bounds);

  // For each dead transform:
  //   1) remove the corresponding matrix
  //   2) record any corresponding image which needs to be released
  // The images found by 2) are handled in two ways:
  //   - by adding them to `images_to_release_` (to ensure that they're properly released even if
  //     the Flatland session is destroyed before releasing the images)
  //   - returned from this function, so that they can be released as soon as the corresponding
  //     release fence is signaled.
  // The images with allocation::kInvalidImageId correspond to filled rects, which do not need to be
  // released.
  std::vector<allocation::GlobalImageId> ProcessDeadTransforms(
      const TransformGraph::TopologyData& data);

  // The dispatcher this Flatland instance is running on.
  async_dispatcher_t* dispatcher() const { return dispatcher_holder_->dispatcher(); }
  std::shared_ptr<utils::DispatcherHolder> dispatcher_holder_;

  // Users are not allowed to use zero as a TransformId or ContentId.
  static constexpr uint64_t kInvalidId = 0;

  // The unique SessionId for this Flatland session. Used to schedule Presents and register
  // UberStructs with the UberStructSystem.
  const scheduling::SessionId session_id_;

  // A Present2Helper to facilitate sendng the appropriate OnFramePresented() callback to FIDL
  // clients when frames are presented to the display.
  scheduling::Present2Helper present2_helper_;

  // A FlatlandPresenter shared between Flatland instances. Flatland uses this interface to get
  // PresentIds when publishing to the UberStructSystem.
  std::shared_ptr<FlatlandPresenter> flatland_presenter_;

  // A link system shared between Flatland instances, so that links can be made between them.
  std::shared_ptr<LinkSystem> link_system_;

  // An UberStructSystem shared between Flatland instances. Flatland publishes local data to the
  // UberStructSystem in order to have it seen by the global render loop.
  std::shared_ptr<UberStructSystem::UberStructQueue> uber_struct_queue_;

  // Used to import Flatland images to external services that Flatland does not have knowledge of.
  // Each importer is used for a different service.
  std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> buffer_collection_importers_;

  // True if there were errors in ParentViewportWatcher or ChildViewWatcher channels.
  bool link_protocol_error_ = false;

  // The number of Present() calls remaining before the client runs out. This value is potentially
  // incremented when OnNextFrameBegin() is called, and decremented by 1 for each Present() call.
  uint32_t present_credits_ = 1;

  // Used for client->Flatland present flow IDs.
  uint64_t present_count_ = 0;

  // Must be managed by a shared_ptr because the implementation uses weak_from_this().
  std::shared_ptr<escher::FenceQueue> fence_queue_ = std::make_shared<escher::FenceQueue>();

  // A map from user-generated ID to global handle. This map constitutes the set of transforms that
  // can be referenced by the user through method calls. Keep in mind that additional transforms may
  // be kept alive through child references.
  std::unordered_map<uint64_t, TransformHandle> transforms_;

  // A graph representing this flatland instance's local transforms and their relationships.
  TransformGraph transform_graph_;

  // A unique transform for this instance, the local_root_, is part of the transform_graph_,
  // and will never be released or changed during the course of the instance's lifetime. This makes
  // it a fixed attachment point for cross-instance Links.
  const TransformHandle local_root_;

  // The transform from the last call to SetRootTransform(). Unlike |local_root_|, this can change
  // over time.
  //
  // Initialize to an invalid handle.
  TransformHandle root_transform_ = TransformHandle(0, 0);

  // A mapping from user-generated ID to the TransformHandle that owns that piece of Content.
  // Attaching Content to a Transform consists of setting one of these "Content Handles" as the
  // priority child of the Transform.
  std::unordered_map<uint64_t, TransformHandle> content_handles_;

  // The set of link operations that are pending a call to Present(). Unlike other operations,
  // whose effects are only visible when a new UberStruct is published, Link destruction operations
  // result in immediate changes in the LinkSystem. To avoid having these changes visible before
  // Present() is called, the actual destruction of Links happens in the following Present().
  std::vector<fit::function<void()>> pending_link_operations_;

  // Wraps a LinkSystem::LinkToChild and the properties currently associated with that link.
  struct LinkToChildData {
    LinkSystem::LinkToChild link;
    fuchsia_ui_composition::ViewportProperties properties;
  };

  // A mapping from Flatland-generated TransformHandle to the LinkToChildData it represents.
  std::unordered_map<TransformHandle, LinkToChildData> links_to_children_;

  // The link from this Flatland instance to our parent.
  std::optional<LinkSystem::LinkToParent> link_to_parent_;

  // Instance name from SetDebugName().
  std::string debug_name_;

  // Represents a geometric transformation as three separate components applied in the following
  // order: translation (relative to the parent's coordinate space), orientation (around the new
  // origin as defined by the translation), and scale (relative to the new rotated origin).
  class MatrixData {
   public:
    void SetTranslation(fuchsia_math::Vec translation);
    void SetOrientation(fuchsia_ui_composition::Orientation orientation);
    void SetScale(fuchsia_math::VecF scale);

    // Returns this geometric transformation as a single 3x3 matrix using the order of operations
    // above: translation, orientation, then scale.
    glm::mat3 GetMatrix() const;

    static float GetOrientationAngle(fuchsia_ui_composition::Orientation orientation);

   private:
    // Applies the translation, then orientation, then scale to the identity matrix.
    void RecomputeMatrix();

    glm::vec2 translation_ = glm::vec2(0.f, 0.f);
    glm::vec2 scale_ = glm::vec2(1.f, 1.f);

    // Counterclockwise rotation angle, in radians.
    float angle_ = 0.f;

    // Recompute and cache the local matrix each time a component is changed to avoid recomputing
    // the matrix for each frame. We expect GetMatrix() to be called far more frequently (roughly
    // once per rendered frame) than the setters are called.
    glm::mat3 matrix_ = glm::mat3(1.f);
  };

  // A geometric transform for each TransformHandle. If not present, that TransformHandle has the
  // identity matrix for its transform.
  std::unordered_map<TransformHandle, MatrixData> matrices_;

  // A map of transform handles to opacity values where the values are strictly in the range
  // [0.f,1.f). 0.f is completely transparent and 1.f is completely opaque. If there is no explicit
  // value associated with a transform handle in this map, then Flatland will consider it to be
  // 1.f by default.
  std::unordered_map<TransformHandle, float> opacity_values_;

  // A map of transform handles to clip regions, where each clip region is a rect to which
  // all child nodes of the transform handle have their rectangular views clipped to.
  std::unordered_map<TransformHandle, TransformClipRegion> clip_regions_;

  // A map of transform handles to hit regions. Each transform's set of hit regions indicate which
  // parts of the transform are user-interactive.
  std::unordered_map<TransformHandle, std::vector<flatland::HitRegion>> hit_regions_;

  // A map of content (image) transform handles to ImageSampleRegion structs which are used
  // to determine the portion of an image that is actually used for rendering.
  std::unordered_map<TransformHandle, ImageSampleRegion> image_sample_regions_;

  // A mapping from Flatland-generated TransformHandle to the ImageMetadata it represents.
  std::unordered_map<TransformHandle, allocation::ImageMetadata> image_metadatas_;

  // Error reporter used for printing debug logs.
  std::unique_ptr<scenic_impl::ErrorReporter> error_reporter_;

  // These images no longer exist in the Flatland session.  They will be released as soon as the
  // corresponding (internally generated) release fence is signaled, indicating that they are no
  // longer in use by the compositor/display-coordinator.  Additionally, if the session is
  // destroyed, this allows the images to be released without waiting for a release fence; see
  // ~Flatland(). The indirection through a shared_ptr is so this can be captured and used in a
  // closure after this Flatland session is destroyed (this happens only in tests, at least when
  // this code was written).
  std::shared_ptr<std::unordered_set<allocation::GlobalImageId>> images_to_release_;

  // Callbacks for registering View-bound protocols.
  fit::function<void(fidl::ServerEnd<fuchsia_ui_views::Focuser>, zx_koid_t)> register_view_focuser_;
  fit::function<void(fidl::ServerEnd<fuchsia_ui_views::ViewRefFocused>, zx_koid_t)>
      register_view_ref_focused_;
  fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::TouchSource>, zx_koid_t)>
      register_touch_source_;
  fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::MouseSource>, zx_koid_t)>
      register_mouse_source_;

  // Helper class that is responsible for managing the Flatland instance's FIDL connection, and also
  // provides an RAII approach to guaranteeing that `destroy_instance_function_` is invoked.
  class BindingData {
   public:
    BindingData(Flatland* flatland, async_dispatcher_t* dispatcher,
                fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end,
                std::function<void()> destroy_instance_function);

    // RAII: invokes `destroy_instance_function_`.
    ~BindingData();

    // Send `OnFramePresented` FIDL event to client.
    void SendOnFramePresented(fuchsia_scenic_scheduling::FramePresentedInfo info);

    // Send `OnNextFrameBegin` FIDL event to client.
    void SendOnNextFrameBegin(uint32_t additional_present_credits,
                              FuturePresentationInfos presentation_infos);

    void CloseConnection(fuchsia_ui_composition::FlatlandError error);

   private:
    // The FIDL binding for this Flatland instance, which references |this| as the implementation
    // and run on |dispatcher_|.
    fidl::ServerBinding<fuchsia_ui_composition::Flatland> binding_;

    // A function that, when called, will destroy this instance. Necessary because an async::Wait
    // can/ only wait on peer channel destruction, not "this" channel destruction, so the
    // FlatlandManager cannot detect if this instance closes |binding_|.
    std::function<void()> destroy_instance_function_;
  };

  std::unique_ptr<BindingData> binding_data_;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_H_
