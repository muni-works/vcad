// Screen-space feature-edge lines for the viewport (RealityKit CustomMaterial).
//
// Each edge segment is one quad: 4 vertices at the segment's endpoints, with
// the segment direction in `normal` and the side (-1/+1) in `uv0.x`. The
// geometry modifier turns it to face the camera, sizes it to a constant pixel
// width, and pulls it a hair toward the camera so it never z-fights the
// surface it lies on. MSAA then gives clean, antialiased ink lines at any zoom.
//
// custom_parameter: x = world size of one pixel per unit distance
//                   (2·tan(fov/2) / viewport height px), y = width in px,
//                   z = ink grey, w = depth pull (fraction of distance).

#include <metal_stdlib>
#include <RealityKit/RealityKit.h>
using namespace metal;

[[visible]]
void edgeLineGeometry(realitykit::geometry_parameters params)
{
    float4 c = params.uniforms().custom_parameter();
    float3 p = params.geometry().model_position();
    float3 dir = params.geometry().normal();
    float side = params.geometry().uv0().x;

    // Work in model space. model_to_view = [A | t] with A = s·R (uniform
    // scale), so A⁻¹ = Aᵀ / s² and the camera sits at -A⁻¹ t.
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

    // Pixel size scales with distance the same way in model and world units.
    float halfW = 0.5 * c.y * c.x * dist;
    params.geometry().set_model_position_offset(perp * side * halfW + view * (c.w * dist));
}

[[visible]]
void edgeLineSurface(realitykit::surface_parameters params)
{
    half g = half(params.uniforms().custom_parameter().z);
    params.surface().set_base_color(half3(g));
    params.surface().set_emissive_color(half3(g));
}

// Coloured variant for selection / hover highlights: the colour comes from
// the material's base-colour tint, so one shader serves every highlight
// colour. Same geometry modifier (custom.z unused here).
[[visible]]
void edgeLineTintSurface(realitykit::surface_parameters params)
{
    half3 c = half3(params.material_constants().base_color_tint());
    params.surface().set_base_color(c);
    params.surface().set_emissive_color(c);
}
