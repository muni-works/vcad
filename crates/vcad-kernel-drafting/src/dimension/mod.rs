//! Dimension annotation system for technical drawings.
//!
//! This module provides a comprehensive system for adding dimensions and
//! geometric tolerancing (GD&T) annotations to projected views.
//!
//! # Dimension Types
//!
//! - [`LinearDimension`] - Horizontal, vertical, aligned, and rotated dimensions
//! - [`AngularDimension`] - Angle measurements between edges or points
//! - [`RadialDimension`] - Radius and diameter dimensions for circles/arcs
//! - [`OrdinateDimension`] - Datum-relative coordinate dimensions
//!
//! # GD&T Support
//!
//! - [`FeatureControlFrame`] - Standard GD&T feature control frames
//! - [`DatumFeatureSymbol`] - Datum feature identification
//!
//! # Example
//!
//! ```ignore
//! use vcad_kernel_drafting::dimension::*;
//!
//! let mut annotations = AnnotationLayer::new();
//!
//! // Add a horizontal dimension between two points
//! annotations.add_horizontal_dimension(
//!     Point2D::new(0.0, 0.0),
//!     Point2D::new(100.0, 0.0),
//!     15.0, // offset above the geometry
//! );
//!
//! // Render all dimensions
//! let rendered = annotations.render_all();
//! ```

mod angular;
mod gdt;
mod geometry_ref;
mod layer;
mod linear;
mod ordinate;
pub mod placement;
mod radial;
mod render;
mod style;

pub use angular::{AngleDefinition, AngularDimension};
pub use gdt::{DatumFeatureSymbol, DatumRef, FeatureControlFrame, GdtSymbol, MaterialCondition};
pub use geometry_ref::GeometryRef;
pub use layer::AnnotationLayer;
pub use linear::{LinearDimension, LinearDimensionType};
pub use ordinate::OrdinateDimension;
pub use radial::RadialDimension;
pub use render::{RenderedArc, RenderedArrow, RenderedDimension, RenderedText, TextAlignment};
pub use style::{ArrowType, DimensionStyle, TextPlacement, ToleranceMode};
