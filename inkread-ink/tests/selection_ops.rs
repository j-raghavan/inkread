//! Integration tests for the lasso selection **mutations** (ADR-INKREAD-0010, M-Lasso-1 FR2/FR3):
//! move / delete / recolor / copy / paste, each reversible through the shared undo/redo history.
//! Exercises only the public `inkread-ink` API.

use inkread_ink::model::{InkColor, InkLayer, InkPoint, Stroke, StrokeId, Tool};

/// Draw + commit a pen stroke through `pts`, returning its id.
fn stroke(layer: &mut InkLayer, color: InkColor, pts: &[(f32, f32)]) -> StrokeId {
    layer.start_stroke(Tool::Pen, color, 0.01, 0).unwrap();
    for &(x, y) in pts {
        layer
            .push_point(InkPoint::new(x, y, 1.0, None, None, 0).unwrap())
            .unwrap();
    }
    layer.finish_stroke().unwrap()
}

/// The points of the stroke with `id`, as `(x, y)` pairs.
fn points_of(layer: &InkLayer, id: StrokeId) -> Vec<(f32, f32)> {
    layer
        .strokes()
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.points.iter().map(|p| (p.x, p.y)).collect())
        .unwrap_or_default()
}

fn color_of(layer: &InkLayer, id: StrokeId) -> InkColor {
    layer.strokes().iter().find(|s| s.id == id).unwrap().color
}

#[test]
fn move_translates_then_undo_restores_exactly() {
    let mut layer = InkLayer::new();
    let a = stroke(
        &mut layer,
        InkColor::default(),
        &[(0.40, 0.40), (0.50, 0.60)],
    );
    let before = points_of(&layer, a);

    let applied = layer.move_strokes(&[a], 0.10, -0.05).expect("moved");
    assert_eq!(applied, (0.10, -0.05));
    assert_eq!(points_of(&layer, a), vec![(0.50, 0.35), (0.60, 0.55)]);

    assert!(layer.undo());
    assert_eq!(points_of(&layer, a), before);
    assert!(layer.redo());
    assert_eq!(points_of(&layer, a), vec![(0.50, 0.35), (0.60, 0.55)]);
}

#[test]
fn move_delta_is_clamped_to_keep_selection_on_page() {
    let mut layer = InkLayer::new();
    let a = stroke(
        &mut layer,
        InkColor::default(),
        &[(0.80, 0.80), (0.90, 0.90)],
    );
    // Asking to move +0.5 right/down would push past 1.0; the delta clamps to 1.0 - 0.90 ≈ 0.10.
    let (cdx, cdy) = layer.move_strokes(&[a], 0.50, 0.50).expect("moved");
    assert!((cdx - 0.10).abs() < 1e-5 && (cdy - 0.10).abs() < 1e-5);
    let pts = points_of(&layer, a);
    assert!((pts[0].0 - 0.90).abs() < 1e-5 && (pts[0].1 - 0.90).abs() < 1e-5);
    assert!((pts[1].0 - 1.0).abs() < 1e-5 && (pts[1].1 - 1.0).abs() < 1e-5);
    // A pure no-op move (already against the edge) returns None and records no edit.
    assert!(layer.move_strokes(&[a], 0.50, 0.50).is_none());
    assert!(layer.undo()); // undo the one recorded move → back to the original position
    let back = points_of(&layer, a);
    assert!((back[1].0 - 0.90).abs() < 1e-5 && (back[1].1 - 0.90).abs() < 1e-5);
    // Below the move sits only the stroke's creation Add (the no-op move recorded nothing).
    assert!(layer.undo()); // undo the Add
    assert!(!layer.can_undo());
}

#[test]
fn delete_many_removes_then_undo_reinserts_in_order() {
    let mut layer = InkLayer::new();
    let a = stroke(&mut layer, InkColor::default(), &[(0.10, 0.10)]);
    let b = stroke(&mut layer, InkColor::default(), &[(0.20, 0.20)]);
    let c = stroke(&mut layer, InkColor::default(), &[(0.30, 0.30)]);

    let removed = layer.delete_strokes(&[a, c]);
    assert_eq!(removed.len(), 2);
    assert_eq!(
        layer.strokes().iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![b]
    );

    assert!(layer.undo()); // one atomic undo restores both, at their original positions
    assert_eq!(
        layer.strokes().iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![a, b, c],
    );
    assert!(layer.redo());
    assert_eq!(
        layer.strokes().iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![b]
    );
}

#[test]
fn recolor_changes_then_undo_restores_each_color() {
    let mut layer = InkLayer::new();
    let red = InkColor::rgba(255, 0, 0, 255);
    let blue = InkColor::rgba(0, 0, 255, 255);
    let a = stroke(&mut layer, red, &[(0.4, 0.4)]);
    let b = stroke(&mut layer, blue, &[(0.5, 0.5)]);

    assert!(layer.recolor_strokes(&[a, b], InkColor::default()));
    assert_eq!(color_of(&layer, a), InkColor::default());
    assert_eq!(color_of(&layer, b), InkColor::default());

    assert!(layer.undo());
    assert_eq!(color_of(&layer, a), red);
    assert_eq!(color_of(&layer, b), blue);
}

#[test]
fn copy_paste_clones_offset_strokes_and_undo_removes_them() {
    let mut layer = InkLayer::new();
    let a = stroke(
        &mut layer,
        InkColor::default(),
        &[(0.40, 0.40), (0.50, 0.50)],
    );
    let clip: Vec<Stroke> = layer.copy_strokes(&[a]);
    assert_eq!(clip.len(), 1);
    assert_eq!(layer.strokes().len(), 1); // copy is non-destructive

    let new_ids = layer.paste_strokes(&clip, 0.10, 0.10);
    assert_eq!(new_ids.len(), 1);
    assert_ne!(new_ids[0], a); // fresh id
    assert_eq!(layer.strokes().len(), 2);
    assert_eq!(
        points_of(&layer, new_ids[0]),
        vec![(0.50, 0.50), (0.60, 0.60)]
    );

    assert!(layer.undo()); // removes the pasted strokes
    assert_eq!(layer.strokes().len(), 1);
    assert_eq!(layer.strokes()[0].id, a);
    assert!(layer.redo());
    assert_eq!(layer.strokes().len(), 2);
}

#[test]
fn cut_is_copy_then_delete_and_paste_round_trips_across_a_fresh_layer() {
    // Models the cross-page clipboard: cut from one layer, paste into another.
    let mut src = InkLayer::new();
    let a = stroke(&mut src, InkColor::default(), &[(0.40, 0.40), (0.50, 0.50)]);
    let clip = src.copy_strokes(&[a]);
    src.delete_strokes(&[a]);
    assert!(src.is_empty());

    let mut dst = InkLayer::new();
    let pasted = dst.paste_strokes(&clip, 0.0, 0.0);
    assert_eq!(pasted.len(), 1);
    assert_eq!(points_of(&dst, pasted[0]), vec![(0.40, 0.40), (0.50, 0.50)]);
}

#[test]
fn empty_or_unknown_ids_are_no_ops() {
    let mut layer = InkLayer::new();
    let a = stroke(&mut layer, InkColor::default(), &[(0.4, 0.4)]);
    let ghost = StrokeId(9999);
    assert!(layer.move_strokes(&[ghost], 0.1, 0.1).is_none());
    assert!(layer.delete_strokes(&[ghost]).is_empty());
    assert!(!layer.recolor_strokes(&[ghost], InkColor::default()));
    assert!(layer.copy_strokes(&[ghost]).is_empty());
    assert_eq!(layer.strokes().len(), 1);
    assert_eq!(layer.strokes()[0].id, a);
    // The only recorded edit is the stroke's own creation; the no-op ops added nothing to undo.
    assert!(layer.undo()); // undoes the Add
    assert!(!layer.can_undo());
}
