//! Sheet composition: placed views + annotations + title block + BOM →
//! [`DrawingSheet`].
//!
//! Shared by the web app's PDF export (`drawingSheetToPdf`) and the native
//! app's drawing workspace, so the sheet a user sees natively is the sheet the
//! web exports.

use serde::{Deserialize, Serialize};

use crate::dimension::RenderedText;
use crate::sheet::{
    BomRow, BomTable, DrawingSheet, LineClass, SheetSize, TitleBlock, TitleBlockFields,
};
use crate::types::{DetailView, Point2D, ProjectedView, SectionView};
use crate::RenderedDimension;

/// Specification for composing a drawing sheet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SheetSpec {
    /// Sheet size; default A4 landscape.
    #[serde(default)]
    pub size: Option<SheetSize>,
    /// Projected views placed on the sheet.
    #[serde(default)]
    pub views: Vec<PlacedProjectedView>,
    /// Section views placed on the sheet.
    #[serde(default)]
    pub sections: Vec<PlacedSectionView>,
    /// Detail views placed on the sheet (already magnified).
    #[serde(default)]
    pub details: Vec<PlacedDetailView>,
    /// Pre-rendered annotations (dimensions, cut lines, GD&T) in sheet mm.
    #[serde(default)]
    pub annotations: Vec<RenderedDimension>,
    /// Center marks and center lines in sheet mm (thin dash-dot).
    #[serde(default)]
    pub center_lines: Vec<RenderedDimension>,
    /// Title block fields; omitted → no title block.
    #[serde(default)]
    pub title_block: Option<TitleBlockFields>,
    /// BOM rows; omitted or empty → no BOM table.
    #[serde(default)]
    pub bom: Option<Vec<BomRow>>,
}

/// A projected view with sheet placement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedProjectedView {
    /// The view.
    pub view: ProjectedView,
    /// Sheet position of the view's bounds center, mm from bottom-left.
    pub center: [f64; 2],
    /// View → sheet scale.
    pub scale: f64,
    /// Caption under the view.
    #[serde(default)]
    pub label: Option<String>,
}

/// A section view with sheet placement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedSectionView {
    /// The view.
    pub view: SectionView,
    /// Sheet position of the view's bounds center, mm.
    pub center: [f64; 2],
    /// View → sheet scale.
    pub scale: f64,
    /// Caption under the view.
    #[serde(default)]
    pub label: Option<String>,
}

/// A detail view with sheet placement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedDetailView {
    /// The (already magnified) view.
    pub view: DetailView,
    /// Sheet position of the view's bounds center, mm.
    pub center: [f64; 2],
    /// Caption under the view.
    #[serde(default)]
    pub label: Option<String>,
}

/// Caption 18 mm below the view: clear of the overall dimension at 10 mm.
fn caption(sheet: &mut DrawingSheet, center: Point2D, half_h: f64, label: &Option<String>) {
    if let Some(label) = label {
        sheet.texts.push(RenderedText::new(
            Point2D::new(center.x, center.y - half_h - 18.0),
            label,
            3.5,
        ));
    }
}

/// Lay out `spec` on a [`DrawingSheet`].
pub fn compose_sheet(spec: &SheetSpec) -> DrawingSheet {
    let mut sheet = DrawingSheet::new(spec.size.unwrap_or(SheetSize::A4));

    for placed in &spec.views {
        let center = Point2D::new(placed.center[0], placed.center[1]);
        sheet.add_projected_view(&placed.view, center, placed.scale);
        let half_h = (placed.view.bounds.max_y - placed.view.bounds.min_y) / 2.0 * placed.scale;
        caption(&mut sheet, center, half_h, &placed.label);
    }

    for placed in &spec.sections {
        let center = Point2D::new(placed.center[0], placed.center[1]);
        sheet.add_section_view(&placed.view, center, placed.scale);
        let b = &placed.view.bounds;
        caption(
            &mut sheet,
            center,
            (b.max_y - b.min_y) / 2.0 * placed.scale,
            &placed.label,
        );
    }

    for placed in &spec.details {
        let center = Point2D::new(placed.center[0], placed.center[1]);
        sheet.add_detail_view(&placed.view, center);
        let b = &placed.view.bounds;
        caption(&mut sheet, center, (b.max_y - b.min_y) / 2.0, &placed.label);
    }

    for rd in &spec.annotations {
        sheet.add_annotation(rd, LineClass::Dimension, Point2D::new(0.0, 0.0));
    }
    for rd in &spec.center_lines {
        sheet.add_annotation(rd, LineClass::Center, Point2D::new(0.0, 0.0));
    }

    let title_block_height = if let Some(fields) = spec.title_block.clone() {
        let block = TitleBlock::new(fields);
        sheet.add_title_block(&block);
        block.height
    } else {
        0.0
    };

    if let Some(rows) = &spec.bom {
        if !rows.is_empty() {
            let table = BomTable { rows: rows.clone() };
            sheet.add_bom_table(&table, title_block_height);
        }
    }

    sheet
}
