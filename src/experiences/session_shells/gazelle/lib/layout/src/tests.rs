// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![allow(unused)]

use layout::*;

#[fuchsia::test]
fn test_min_max_size() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> = Box::new(
        FloatingLayout::new()
            .with_default_size(SizeU { width: 200, height: 100 })
            .with_min_size(SizeU { width: 40, height: 40 })
            .with_max_size(SizeU { width: 1000, height: 1000 }),
    );

    layout.add(TestFrame::new(42));
    layout.add(TestFrame::new_size(43, 5, 5));
    layout.add(TestFrame::new_size(44, 5000, 5000));
    assert_eq!(layout.iter().count(), 3);

    let frame_offset = FloatingLayout::<u64, TestFrame>::FLOATING_FRAME_OFFSET as f32;

    for frame in layout.iter() {
        let rect = frame.get_rect().unwrap();
        match frame.get_id() {
            // Test default size.
            42 => assert_eq!(rect, RectF { x: 0.0, y: 0.0, width: 200.0, height: 100.0 }),
            // Test min size.
            43 => assert_eq!(
                rect,
                RectF { x: frame_offset, y: frame_offset, width: 40.0, height: 40.0 }
            ),
            // Test max size.
            44 => assert_eq!(
                rect,
                RectF {
                    x: frame_offset * 2.0,
                    y: frame_offset * 2.0,
                    width: 1000.0,
                    height: 1000.0
                }
            ),
            id => panic!("Found unknown frame with id: {:?}", id),
        }
    }
}

#[fuchsia::test]
fn test_add_remove_frame() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> = Box::new(FloatingLayout::new());

    layout.add(TestFrame::new(42));
    layout.add(TestFrame::new_size(43, 5, 5));
    layout.add(TestFrame::new_size(44, 5000, 5000));
    assert_eq!(layout.iter().count(), 3);

    // Last frame is always active.
    assert_eq!(44, layout.get_active().unwrap());

    // Remove last frame.
    layout.remove(44);
    assert_eq!(layout.iter().count(), 2);

    // Last frame is always active.
    assert_eq!(43, layout.get_active().unwrap());

    layout.clear();
    assert_eq!(layout.iter().count(), 0);
    assert!(layout.get_active().is_none());
}

#[fuchsia::test]
fn test_set_state() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> = Box::new(FloatingLayout::new());

    layout.add(TestFrame::new(42));
    layout.add(TestFrame::new_size(43, 5, 5));
    layout.add(TestFrame::new_size(44, 5000, 5000));
    assert_eq!(layout.iter().count(), 3);

    // Setting Fullscreen state on any frame should mark it active.
    layout.set_state(43, FrameState::Fullscreen);
    assert_eq!(43, layout.get_active().unwrap());
    assert_eq!(
        layout.get_frame(43).and_then(|f| Some(f.get_state())),
        Some(FrameState::Fullscreen)
    );

    // Setting a frame active while another frame is fullscreen should toggle back to normal.
    layout.set_active(SetActiveFrame::Frame(42));
    assert_eq!(42, layout.get_active().unwrap());
    assert_eq!(layout.get_frame(43).and_then(|f| Some(f.get_state())), Some(FrameState::Normal));
}

#[fuchsia::test]
fn test_set_active() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> = Box::new(FloatingLayout::new());

    layout.add(TestFrame::new(42));
    layout.add(TestFrame::new_size(43, 5, 5));
    layout.add(TestFrame::new_size(44, 5000, 5000));
    assert_eq!(layout.iter().count(), 3);

    layout.set_active(SetActiveFrame::Frame(42));
    assert_eq!(42, layout.get_active().unwrap());

    layout.set_active(SetActiveFrame::NextFrame);
    assert_eq!(43, layout.get_active().unwrap());

    layout.set_active(SetActiveFrame::PrevFrame);
    assert_eq!(42, layout.get_active().unwrap());
}

#[fuchsia::test]
fn test_move_frame() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> =
        Box::new(FloatingLayout::new().with_default_size(SizeU { width: 200, height: 100 }));

    layout.add(TestFrame::new(42));
    assert_eq!(layout.iter().count(), 1);

    // Drag the titlebar area.
    layout.on_pointer(Pointer::Down { point: PointF { x: 10.0, y: 10.0 } });
    layout.on_pointer(Pointer::Move {
        point: PointF { x: 20.0, y: 20.0 },
        delta: PointF { x: 10.0, y: 10.0 },
    });

    assert_eq!(
        layout.get_frame(42).and_then(|f| f.get_rect().ok()),
        Some(RectF { x: 10.0, y: 10.0, width: 200.0, height: 100.0 })
    );
}

#[fuchsia::test]
fn test_resize_frame() {
    let mut layout: Box<dyn Layout<u64, TestFrame>> =
        Box::new(FloatingLayout::new().with_default_size(SizeU { width: 200, height: 100 }));

    layout.add(TestFrame::new(42));
    assert_eq!(layout.iter().count(), 1);

    // Drag the right border right to increase width.
    let mut point = PointF {
        x: 200.0 + (TestFrame::BORDER_THICKNESS / 2) as f32,
        y: 10.0 + TestFrame::TITLEBAR_HEIGHT as f32,
    };
    layout.on_pointer(Pointer::Down { point });

    point.x += 10.0;
    layout.on_pointer(Pointer::Move { point, delta: PointF { x: 10.0, y: 0.0 } });

    assert_eq!(
        layout.get_frame(42).and_then(|f| f.get_rect().ok()),
        Some(RectF { x: 0.0, y: 0.0, width: 210.0, height: 100.0 })
    );
}

#[derive(Debug)]
struct TestFrame {
    id: u64,
    rect: Option<RectF>,
    state: FrameState,
}

impl TestFrame {
    pub const TITLEBAR_HEIGHT: u32 = 32;
    pub const BORDER_THICKNESS: i32 = 10;

    pub fn new(id: u64) -> Self {
        TestFrame { id, rect: None, state: FrameState::Normal }
    }

    pub fn new_size(id: u64, width: u32, height: u32) -> Self {
        TestFrame {
            id,
            rect: Some(RectF { x: 0.0, y: 0.0, width: width as f32, height: height as f32 }),
            state: FrameState::Normal,
        }
    }
}

impl Frame<u64> for TestFrame {
    fn get_id(&self) -> u64 {
        self.id
    }

    fn get_parent(&self) -> Option<u64> {
        None
    }

    fn get_alignment(&self) -> Option<Alignment> {
        None
    }

    fn get_initial_size(&self) -> Option<SizeU> {
        self.rect.map(|r| SizeU { width: r.width as u32, height: r.height as u32 })
    }

    fn get_rect(&self) -> Result<RectF, LayoutError<u64>> {
        self.rect.ok_or(LayoutError::Failed { source: anyhow::anyhow!("Rect not set") })
    }

    fn set_rect(&mut self, rect: RectF) -> Result<LayoutChange, LayoutError<u64>> {
        self.rect = Some(rect);
        Ok(LayoutChange::None)
    }

    fn get_state(&self) -> FrameState {
        self.state
    }

    fn set_state(&mut self, state: FrameState) -> Result<LayoutChange, LayoutError<u64>> {
        self.state = state;
        Ok(LayoutChange::None)
    }

    fn set_active(&mut self, active: bool) -> Result<LayoutChange, LayoutError<u64>> {
        Ok(LayoutChange::None)
    }

    fn hittest(&self, point: PointF) -> Option<HitTarget<u64>> {
        // Apply border thickness to get frame and body rects.
        let (frame_rect, body_rect) = if TestFrame::BORDER_THICKNESS > 0 {
            (self.rect.unwrap().inflate(TestFrame::BORDER_THICKNESS as f32), self.rect.unwrap())
        } else {
            (self.rect.unwrap(), self.rect.unwrap().inflate(TestFrame::BORDER_THICKNESS as f32))
        };

        match hittest(point, frame_rect, body_rect, TestFrame::TITLEBAR_HEIGHT) {
            HitArea::Outside => None,
            hit_area => Some(HitTarget { id: self.id, hit_area, point, hit_data: None }),
        }
    }

    fn hover(
        &mut self,
        point: PointF,
        hit_area: HitArea,
    ) -> Result<LayoutChange, LayoutError<u64>> {
        Ok(LayoutChange::None)
    }
}
