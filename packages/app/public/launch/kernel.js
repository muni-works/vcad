// Figure 4 on the launch page: the gradient of a drilled plate's volume with
// respect to the hole radius, computed the way vcad-kernel-diff does it, on
// the frozen tessellation, and checked against central differences.
// External file because vcad.io's CSP blocks inline scripts.
(() => {
  const W = 60, D = 40, t = 6;          // the plate from the first-part sample, in mm
  const N = 64;                         // segments around the hole in the frozen mesh

  // Volume of the meshed part: the hole is an N-gon, so its area is
  // (N/2) r^2 sin(2π/N), not π r^2. The kernel differentiates this mesh.
  const meshVolume = (r) => t * (W * D - 0.5 * N * r * r * Math.sin(2 * Math.PI / N));
  // d/dr of the meshed volume: what the seam returns (node velocities dotted
  // with ∂V/∂x, which for this part collapses to the closed form below).
  const seamGradient = (r) => -t * N * r * Math.sin(2 * Math.PI / N);
  const exactGradient = (r) => -2 * Math.PI * r * t;   // the smooth part, for reference

  const fmt = (x, d = 2) => x.toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d });

  document.addEventListener("DOMContentLoaded", () => {
    const slider = document.getElementById("g-r");
    if (!slider) return;
    const out = (id, v) => { const el = document.getElementById(id); if (el) el.textContent = v; };
    const hole = document.getElementById("g-hole");

    const update = () => {
      const r = parseFloat(slider.value);
      const h = 1e-4;
      const fd = (meshVolume(r + h) - meshVolume(r - h)) / (2 * h);
      const g = seamGradient(r);
      out("g-r-v", fmt(r, 1) + " mm");
      out("g-vol", fmt(meshVolume(r), 1) + " mm³");
      out("g-seam", fmt(g, 3) + " mm²");
      out("g-fd", fmt(fd, 3) + " mm²");
      out("g-err", (Math.abs(fd - g) / Math.abs(g)).toExponential(1));
      out("g-smooth", fmt(exactGradient(r), 3) + " mm²");
      if (hole) {
        // redraw the hole as the N-gon the mesh actually holds
        const cx = 150, cy = 80, s = 2.4;
        let d = "";
        for (let i = 0; i <= N; i++) {
          const a = (2 * Math.PI * i) / N;
          d += (i ? "L" : "M") + (cx + s * r * Math.cos(a)).toFixed(2) + " " + (cy + s * r * Math.sin(a)).toFixed(2);
        }
        hole.setAttribute("d", d + "Z");
        const arrow = document.getElementById("g-arrow");
        if (arrow) arrow.setAttribute("x1", (cx + s * r).toFixed(2));
        if (arrow) arrow.setAttribute("x2", (cx + s * r + 16).toFixed(2));
      }
    };
    slider.addEventListener("input", update);
    update();
  });
})();
