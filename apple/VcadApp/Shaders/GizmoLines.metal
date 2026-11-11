// Screen-space lines for the transform gizmo and direct-manipulation handles.
//
// Same quad layout as EdgeLines.metal (endpoints twice, segment direction in
// `normal`, side in `uv0.x`), but coloured: the ink comes from the material's
// base-colour tint and opacity, so one shader draws every axis, ring and
// hover state at a constant pixel width.
//
// custom_parameter: x = world size of one pixel per unit distance,
//                   y = width in px, z = unused, w = depth pull.

#include <metal_stdlib>
#include <RealityKit/RealityKit.h>
using namespace metal;

[[visible]]
void gizmoLineGeometry(realitykit::geometry_parameters params)
{
    float4 c = params.uniforms().custom_parameter();
    float3 p = params.geometry().model_position();
    float3 dir = params.geometry().normal();
    float side = params.geometry().uv0().x;

    float4x4 m2v = params.uniforms().model_to_view();
    float3x3 a = float3x3(m2v[0].xyz, m2v[1].xyz, m2v[2].xyz);
    float s2 = max(dot(a[0], a[0]), 1e-12);
    float3 cam = -(transpose(a) * m2v[3].xyz) / s2;

    float3 toCam = cam - p;
    float dist = max(length(toCam), 1e-9);
    float3 view = toCam / dist;
    float3 perp = cross(dir, view);
    float pl = length(perp);
    perp = pl > 1e-9 ? perp / pl : float3(0.0);

    float halfW = 0.5 * c.y * c.x * dist;
    params.geometry().set_model_position_offset(perp * side * halfW + view * (c.w * dist));
}

[[visible]]
void gizmoLineSurface(realitykit::surface_parameters params)
{
    half3 ink = half3(params.material_constants().base_color_tint());
    half op = half(params.material_constants().opacity_scale());
    params.surface().set_base_color(ink);
    params.surface().set_emissive_color(ink);
    params.surface().set_opacity(op);
}
