//! Levenberg-Marquardt solver for constraint systems.
//!
//! The solver minimizes the sum of squared residuals (errors) from all
//! constraints by iteratively adjusting the parameter values.

use crate::constraint::Constraint;
use crate::entity::{EntityId, SketchEntity};
use crate::symbolic::CompiledSystem;
use slotmap::SlotMap;

// The Levenberg-Marquardt driver, its `LeastSquares` trait and its config
// and result types live in `tang-optim`; the names here are the ones this
// crate has always exported.
pub use tang_optim::{
    levenberg_marquardt, LeastSquares, LmConfig as SolverConfig, LmResult as SolveResult,
    LmStatus as SolveStatus,
};

/// Run the Levenberg-Marquardt solver.
///
/// # Arguments
///
/// * `constraints` - The constraints to satisfy
/// * `params` - Initial parameter values (modified in place)
/// * `entities` - Entity definitions
/// * `config` - Solver configuration
///
/// # Returns
///
/// The solve result containing final parameters and convergence info.
pub fn solve(
    constraints: &[Constraint],
    params: &mut [f64],
    entities: &SlotMap<EntityId, SketchEntity>,
    config: &SolverConfig,
) -> SolveResult {
    if constraints.is_empty() {
        return SolveResult {
            parameters: params.to_vec(),
            residual_norm: 0.0,
            iterations: 0,
            converged: true,
            status: SolveStatus::NoConstraints,
        };
    }

    if params.is_empty() {
        let system = CompiledSystem::build(constraints, entities, 0);
        return SolveResult {
            parameters: vec![],
            residual_norm: system.residual_norm_squared(params).sqrt(),
            iterations: 0,
            converged: false,
            status: SolveStatus::NoParameters,
        };
    }

    // Build the symbolic system once (depends only on constraint topology),
    // then hand it to the generic Levenberg-Marquardt driver.
    let system = CompiledSystem::build(constraints, entities, params.len());
    levenberg_marquardt(&system, params, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraint::EntityRef;
    use crate::entity::{SketchLine, SketchPoint};

    #[test]
    fn test_solve_coincident() {
        // Two points that should become coincident
        let mut entities = SlotMap::with_key();
        let p1 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 0,
            param_y: 1,
        }));
        let p2 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 2,
            param_y: 3,
        }));

        // Start with p1 at (0, 0), p2 at (10, 5)
        let mut params = vec![0.0, 0.0, 10.0, 5.0];

        let constraints = vec![Constraint::Coincident {
            point_a: EntityRef::Point(p1),
            point_b: EntityRef::Point(p2),
        }];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged, "Solver should converge");
        // Points should be at the same location
        let dx = params[2] - params[0];
        let dy = params[3] - params[1];
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist < 1e-6,
            "Points should be coincident, distance = {dist}"
        );
    }

    #[test]
    fn test_solve_horizontal() {
        let mut entities = SlotMap::with_key();
        let p1 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 0,
            param_y: 1,
        }));
        let p2 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 2,
            param_y: 3,
        }));
        let line = entities.insert(SketchEntity::Line(SketchLine { start: p1, end: p2 }));

        // Start with diagonal line from (0, 0) to (10, 5)
        let mut params = vec![0.0, 0.0, 10.0, 5.0];

        let constraints = vec![Constraint::Horizontal { line }];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged, "Solver should converge");
        // Line should be horizontal: start.y == end.y
        let dy = (params[3] - params[1]).abs();
        assert!(dy < 1e-6, "Line should be horizontal, dy = {dy}");
    }

    #[test]
    fn test_solve_distance() {
        let mut entities = SlotMap::with_key();
        let p1 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 0,
            param_y: 1,
        }));
        let p2 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 2,
            param_y: 3,
        }));

        // Start with p1 at (0, 0), p2 at (10, 0)
        let mut params = vec![0.0, 0.0, 10.0, 0.0];

        let constraints = vec![Constraint::Distance {
            point_a: EntityRef::Point(p1),
            point_b: EntityRef::Point(p2),
            distance: 5.0,
        }];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged, "Solver should converge");
        // Distance should be 5
        let dx = params[2] - params[0];
        let dy = params[3] - params[1];
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "Distance should be 5, got {dist}"
        );
    }

    #[test]
    fn test_solve_fixed_point() {
        let mut entities = SlotMap::with_key();
        let p1 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 0,
            param_y: 1,
        }));

        // Start at (10, 20)
        let mut params = vec![10.0, 20.0];

        let constraints = vec![Constraint::Fixed {
            point: EntityRef::Point(p1),
            x: 5.0,
            y: 3.0,
        }];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged, "Solver should converge");
        assert!((params[0] - 5.0).abs() < 1e-6, "X should be 5");
        assert!((params[1] - 3.0).abs() < 1e-6, "Y should be 3");
    }

    #[test]
    fn test_solve_perpendicular() {
        let mut entities = SlotMap::with_key();
        let p1 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 0,
            param_y: 1,
        }));
        let p2 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 2,
            param_y: 3,
        }));
        let p3 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 4,
            param_y: 5,
        }));
        let p4 = entities.insert(SketchEntity::Point(SketchPoint {
            param_x: 6,
            param_y: 7,
        }));
        let line1 = entities.insert(SketchEntity::Line(SketchLine { start: p1, end: p2 }));
        let line2 = entities.insert(SketchEntity::Line(SketchLine { start: p3, end: p4 }));

        // Line1: (0,0) to (10,0) - horizontal
        // Line2: (5,0) to (5,10) - vertical (already perpendicular)
        // Start with non-perpendicular lines
        let mut params = vec![0.0, 0.0, 10.0, 2.0, 5.0, 0.0, 8.0, 10.0];

        let constraints = vec![Constraint::Perpendicular {
            line_a: line1,
            line_b: line2,
        }];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged, "Solver should converge");

        // Check perpendicularity: dot product of direction vectors should be 0
        let d1x = params[2] - params[0];
        let d1y = params[3] - params[1];
        let d2x = params[6] - params[4];
        let d2y = params[7] - params[5];
        let len1 = (d1x * d1x + d1y * d1y).sqrt();
        let len2 = (d2x * d2x + d2y * d2y).sqrt();
        let dot = (d1x * d2x + d1y * d2y) / (len1 * len2);
        assert!(
            dot.abs() < 1e-6,
            "Lines should be perpendicular, dot = {dot}"
        );
    }

    #[test]
    fn test_no_constraints() {
        let entities: SlotMap<EntityId, SketchEntity> = SlotMap::with_key();
        let mut params = vec![1.0, 2.0];
        let constraints: Vec<Constraint> = vec![];

        let result = solve(
            &constraints,
            &mut params,
            &entities,
            &SolverConfig::default(),
        );

        assert!(result.converged);
        assert_eq!(result.status, SolveStatus::NoConstraints);
    }
}
