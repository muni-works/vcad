// Parametric floorplan + hand pre-routes for ice-viz (v3). Frame: mm, y-up, rotation CCW degrees.
// Signal flow left -> right: USB-C -> RP2040 -> config flash -> iCE40 -> LCD header.
export function layout(W, H, o = {}) {
  const P = [], T = [], X = [];
  const put = (ref, x, y, rotation = 0) => P.push({ ref, x: +x.toFixed(3), y: +y.toFixed(3), rotation });
  const grp = (a, items) => items.forEach(([r, dx, dy, rot]) => put(r, a[0] + dx, a[1] + dy, rot));
  const tr = (a, net, pts, width = 0.25, layer = "FCu") =>
    T.push({ net, layer, width, points: pts.map(([x, y]) => ({ x: +(a[0] + x).toFixed(3), y: +(a[1] + y).toFixed(3) })) });
  const post = (a, net, pts, width = 0.25, layer = "FCu") => { tr(a, net, pts, width, layer); X.push(T.pop()); };
  const j4 = o.j4 ?? [W - 12.96, H - 1.7];
  const u3 = o.u3 ?? [j4[0] - 2.54, H - 11];
  const u1 = o.u1 ?? [u3[0] - 16, 17];
  // LCD header: right edge, same orientation as the original board
  put("J2", W - 1.8, o.j2y ?? 19.5, 0);
  // FPGA I/O header on the top edge, directly above the FPGA's FIO side
  put("J4", j4[0], j4[1], 90);
  // iCE40: FIO side up (J4), SPI/touch side left (toward RP2040/flash), LCD side down
  grp(u3, [
    ["U3", 0, 0, 90],
    ["C11", -3.0, -5.6, 270], ["C19", -1.5, -5.6, 270],    // 3V3 pin 1, 1V2 pin 5
    ["C12", -5.7, 3.2, 180],                                // 3V3 pins 22/24
    ["C13", -1.25, 6.0, 90], ["C14", 0.5, 6.0, 90],         // 3V3 pin 33, 1V2 pins 29/30
    ["LED2", 6.2, 1.75, 180], ["R9", 8.9, 1.75, 180],
    // 1V2 LDO directly under the core-supply pin
    ["U6", -2.3, -10.4, 180], ["C10", -2.3, -15.2, 270],
  ]);
  // 1V2: pin 5 -> C19 -> LDO output, and a ring inside the QFN (between pads and EP) to pins 29/30
  // (a net with hand copper is left alone by route_nets, so 1V2 is routed completely here)
  tr(u3, "1V2", [[-2.3, -14.2], [-2.3, -9.65], [-1.6, -9.65], [-1.6, -7.95], [-0.75, -7.1], [-0.75, -2.5], [2.5, -2.5], [2.5, 2.5], [0.25, 2.5], [0.25, 5.0]]);
  tr(u3, "1V2", [[0.75, 2.5], [0.75, 5.0]]);
  tr(u3, "1V2", [[-0.75, -4.5], [-1.3, -4.8]]);
  post(u3, "3V3", [[-1.25, 3.45], [-1.25, 5.1]]);
  // FPGA config flash between RP2040 and FPGA (shared SPI bus)
  grp(o.u4 ?? [u1[0] + 8, u1[1] + 8], [["U4", 0, 0, 270], ["C18", 0.6, -5.1, 270]]);
  // RP2040: USB/QSPI side up, ICE SPI side right, crystal/SWD side down
  grp(u1, [
    ["U1", 0, 0, 180],
    ["U2", 2.0, 11.0, 180], ["C17", -2.33, 9.09, 180],
    ["C3", -5.4, 5.3, 180], ["C7", -2.6, 6.1, 90], ["C5", -0.5, 6.1, 90],
    ["C1", 5.5, -0.4, 270], ["C2", 6.0, -3.6, 0], ["R10", 9.1, -3.6, 0],
    ["C6", -1.2, -7.1, 270], ["C4", -0.05, -7.1, 270],
    ["Y1", 2.4, -7.4, 270], ["R5", 1.3, -10.0, 0], ["C15", 5.4, -5.6, 0], ["C16", 5.4, -8.5, 0],
    ["R8", -6.5, -0.75, 180], ["LED1", -9.2, -0.75, 180],
    ["SW2", 2.0, 16.2, 180], ["R7", 7.1, 14.6, 90],
    ["SW1", -8.0, -11.0, 180], ["R6", -5.25, -8.8, 0],
  ]);
  // RP2040 core supply: ring inside the QFN joining pins 23, 45, 50
  // and out to its two decouplers (C6 below pin 23, C7 above pin 45). Routed completely by hand.
  tr(u1, "RP_DVDD", [[-1.1, -6.0], [-0.75, -5.3], [-0.75, -2.9], [-2.9, -2.9], [-2.9, 2.9], [0.25, 2.9], [0.25, 3.95]]);
  tr(u1, "RP_DVDD", [[-2.25, 2.9], [-2.25, 5.2]]);
  // patch applied only if the autorouter strands it: 3V3 pin 10 -> its decoupler
  post(u1, "3V3", [[3.95, 1.25], [4.7, 1.25], [5.4, 0.55]]);
  put("J3", o.j3x ?? u1[0] - 9.5, 1.8, 270);
  // USB-C + 3V3 LDO on the left edge
  grp(o.j1 ?? [4.5, 26], [["J1", 0, 0, 90], ["R1", 4.8, -1.1, 0], ["R3", 4.8, 0.2, 0], ["R4", 4.8, 1.5, 0], ["R2", -1.9, 6.6, 90]]);
  grp(o.u5 ?? [8.3, H - 4.5], [["U5", 0, 0, 0], ["C9", 5.6, 1.7, 0], ["C8", 0.3, -4.75, 270]]);
  return { P, T, X };
}
