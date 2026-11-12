// Renders the DMG window background (a drafting sheet) at 1x and 2x.
//   swift apple/VcadApp/scripts/dmg-background.swift <out-dir>
// Writes dmg-background.png + dmg-background@2x.png. The layout must match the
// create-dmg window size and icon positions in release.sh.
import AppKit

let W: CGFloat = 660, H: CGFloat = 420
let appX: CGFloat = 180, linkX: CGFloat = 480, iconY: CGFloat = 190 // icon centres, top-left origin
let ground = NSColor(srgbRed: 0.075, green: 0.078, blue: 0.09, alpha: 1)
let orange = NSColor(srgbRed: 0.93, green: 0.45, blue: 0.22, alpha: 1)

for f in ["NewYork.ttf", "NewYorkItalic.ttf"] {
  CTFontManagerRegisterFontsForURL(URL(fileURLWithPath: "/System/Library/Fonts/\(f)") as CFURL, .process, nil)
}
func serifFont(_ size: CGFloat, italic: Bool) -> NSFont {
  let d = NSFontDescriptor(fontAttributes: [.family: "New York"]).withSymbolicTraits(italic ? .italic : [])
  return NSFont(descriptor: d, size: size) ?? .systemFont(ofSize: size)
}
func font(_ name: String, _ size: CGFloat, _ fallback: NSFont) -> NSFont { NSFont(name: name, size: size) ?? fallback }

func render(scale: CGFloat) -> Data {
  let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
                             bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                             colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
  rep.size = NSSize(width: W, height: H)
  NSGraphicsContext.saveGraphicsState()
  let base = NSGraphicsContext(bitmapImageRep: rep)!
  let ctx = base.cgContext
  ctx.translateBy(x: 0, y: H); ctx.scaleBy(x: 1, y: -1)
  NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: true)
  let px = 1 / scale

  ground.setFill(); NSRect(x: 0, y: 0, width: W, height: H).fill()
  NSGradient(colors: [NSColor(white: 1, alpha: 0.07), NSColor(white: 1, alpha: 0)])!
    .draw(fromCenter: NSPoint(x: W / 2, y: iconY - 10), radius: 0,
          toCenter: NSPoint(x: W / 2, y: iconY - 10), radius: 340, options: [])

  // Drafting grid: fine 10 pt, major 50 pt.
  let gx0: CGFloat = 30, gy0: CGFloat = 30, gx1 = W - 30, gy1 = H - 70
  for (step, alpha) in [(CGFloat(10), CGFloat(0.03)), (CGFloat(50), CGFloat(0.065))] {
    NSColor(white: 1, alpha: alpha).setFill()
    var x = gx0; while x <= gx1 + 0.1 { NSRect(x: x, y: gy0, width: px, height: gy1 - gy0).fill(); x += step }
    var y = gy0; while y <= gy1 + 0.1 { NSRect(x: gx0, y: y, width: gx1 - gx0, height: px).fill(); y += step }
  }
  // Let the grid dissolve toward the sheet edge.
  NSGradient(colorsAndLocations: (ground.withAlphaComponent(0), 0), (ground.withAlphaComponent(0), 0.45), (ground, 1))!
    .draw(fromCenter: NSPoint(x: W / 2, y: (gy0 + gy1) / 2), radius: 0,
          toCenter: NSPoint(x: W / 2, y: (gy0 + gy1) / 2), radius: 360, options: [.drawsAfterEndingLocation])

  // Registration marks at the sheet corners.
  NSColor(white: 1, alpha: 0.3).setStroke()
  for (cx, cy) in [(gx0, gy0), (gx1, gy0), (gx0, gy1), (gx1, gy1)] {
    let p = NSBezierPath(); p.lineWidth = px
    p.move(to: NSPoint(x: cx - 6, y: cy)); p.line(to: NSPoint(x: cx + 6, y: cy))
    p.move(to: NSPoint(x: cx, y: cy - 6)); p.line(to: NSPoint(x: cx, y: cy + 6))
    p.stroke()
  }

  // Dimension line, app → Applications: extension tick, line, arrowhead.
  let x0 = appX + 84, x1 = linkX - 84, dy = iconY
  orange.setStroke(); orange.setFill()
  let d = NSBezierPath(); d.lineWidth = 1.25
  d.move(to: NSPoint(x: x0, y: dy - 7)); d.line(to: NSPoint(x: x0, y: dy + 7))
  d.move(to: NSPoint(x: x0, y: dy)); d.line(to: NSPoint(x: x1 - 8, y: dy)); d.stroke()
  let head = NSBezierPath()
  head.move(to: NSPoint(x: x1, y: dy)); head.line(to: NSPoint(x: x1 - 12, y: dy - 4.5))
  head.line(to: NSPoint(x: x1 - 12, y: dy + 4.5)); head.close(); head.fill()

  func text(_ s: String, _ f: NSFont, _ c: NSColor, kern: CGFloat = 0, x: CGFloat, y: CGFloat, align: Int = -1) {
    let a = NSAttributedString(string: s, attributes: [.font: f, .foregroundColor: c, .kern: kern])
    let w = a.size().width
    a.draw(at: NSPoint(x: align < 0 ? x : (align == 0 ? x - w / 2 : x - w), y: y))
  }
  let mono = font("SFMono-Regular", 9, .monospacedSystemFont(ofSize: 9, weight: .regular))
  let serif = serifFont(25, italic: false)
  let italic = serifFont(13, italic: true)

  // Label tags under each icon. Finder draws icon labels black in light mode and
  // white in dark mode, ignoring the background, so the tags are a mid tone
  // (~#8f8f8f) that keeps both readable (black ≈ 6.5:1, white ≈ 3.2:1).
  for (cx, w) in [(appX, CGFloat(64)), (linkX, CGFloat(104))] {
    NSColor(white: 1, alpha: 0.53).setFill()
    NSBezierPath(roundedRect: NSRect(x: cx - w / 2, y: iconY + 69, width: w, height: 19), xRadius: 4, yRadius: 4).fill()
  }

  text("DRAG TO INSTALL", mono, NSColor(white: 1, alpha: 0.55), kern: 2.4, x: (x0 + x1) / 2, y: dy - 24, align: 0)

  // Title block.
  NSColor(white: 1, alpha: 0.14).setFill(); NSRect(x: gx0, y: H - 56, width: gx1 - gx0, height: px).fill()
  text("vcad", serif, NSColor(white: 1, alpha: 0.94), kern: -0.3, x: gx0, y: H - 48)
  text("Parametric CAD with an open kernel", italic, NSColor(white: 1, alpha: 0.5), x: gx0 + 60, y: H - 38)
  text("SHEET 1/1  ·  SCALE 1:1  ·  macOS 15+", mono, NSColor(white: 1, alpha: 0.36), kern: 1.4, x: gx1, y: H - 35, align: 1)

  NSGraphicsContext.restoreGraphicsState()
  return rep.representation(using: .png, properties: [:])!
}

let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
try! render(scale: 1).write(to: URL(fileURLWithPath: "\(out)/dmg-background.png"))
try! render(scale: 2).write(to: URL(fileURLWithPath: "\(out)/dmg-background@2x.png"))
