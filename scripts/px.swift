#!/usr/bin/env swift
import AppKit
import CoreGraphics
import Foundation

// PNG [x y width height [scale]] measures a rect in points, at the given scale.
// PNG --video WxH measures the centered aspect-fit video rect in a screenshot.
// --assert-pattern fails for black video or a missing amber test-pattern stripe.
// --assert-ui fails for a blank UI or one without its accent: the lime of the
// logo (current apps) or amber (builds before the redesign).
func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}
var args = Array(CommandLine.arguments.dropFirst())
let assertPattern = args.contains("--assert-pattern")
let assertUI = args.contains("--assert-ui")
let quadrants = args.contains("--quadrants")
args.removeAll { $0 == "--assert-pattern" || $0 == "--assert-ui" || $0 == "--quadrants" }
guard let path = args.first,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
else { fail("Usage: px.swift image.png [x y width height [scale] | --video WxH] [--assert-pattern]") }
let width = image.width, height = image.height
var rect = CGRect(x: 0, y: 0, width: width, height: height)
if args.count == 3 && args[1] == "--video" {
    let size = args[2].split(separator: "x").compactMap { Double($0) }
    guard size.count == 2, size.allSatisfy({ $0.isFinite && $0 > 0 }) else { fail("Invalid video size") }
    let scale = min(Double(width) / size[0], Double(height) / size[1])
    rect = CGRect(x: (Double(width) - size[0] * scale) / 2,
                  y: (Double(height) - size[1] * scale) / 2,
                  width: size[0] * scale, height: size[1] * scale)
} else if args.count == 5 || args.count == 6 {
    let numbers = args.dropFirst().compactMap(Double.init)
    guard numbers.count == args.count - 1, numbers.allSatisfy(\.isFinite) else { fail("Invalid rectangle") }
    let scale = numbers.count == 5 ? numbers[4] : 1
    guard scale > 0 else { fail("Scale must be positive") }
    rect = CGRect(x: numbers[0] * scale, y: numbers[1] * scale,
                  width: numbers[2] * scale, height: numbers[3] * scale)
} else if args.count != 1 {
    fail("Invalid arguments")
}
guard rect.width > 0, rect.height > 0, rect.minX >= 0, rect.minY >= 0,
      rect.maxX <= CGFloat(width), rect.maxY <= CGFloat(height) else { fail("Rectangle is outside image") }
var pixels = [UInt8](repeating: 0, count: width * height * 4)
guard let space = CGColorSpace(name: CGColorSpace.sRGB) else { fail("Cannot create sRGB space") }
pixels.withUnsafeMutableBytes { bytes in
    guard let context = CGContext(data: bytes.baseAddress, width: width, height: height,
                                  bitsPerComponent: 8, bytesPerRow: width * 4, space: space,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                                    | CGBitmapInfo.byteOrder32Big.rawValue) else { fail("Cannot read PNG") }
    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
}
var luminance = 0.0, black = 0, amber = 0, lime = 0, count = 0
for y in Int(rect.minY)..<Int(rect.maxY) {
    for x in Int(rect.minX)..<Int(rect.maxX) {
        let i = (y * width + x) * 4
        let r = Double(pixels[i]), g = Double(pixels[i + 1]), b = Double(pixels[i + 2])
        luminance += 0.2126 * r + 0.7152 * g + 0.0722 * b
        if max(r, max(g, b)) < 16 { black += 1 }
        if r > 200 && g > 80 && g < 170 && b < 90 { amber += 1 }
        if r > 190 && g > 220 && b < 140 { lime += 1 }
        count += 1
    }
}
guard count > 0 else { fail("Empty rectangle") }
let mean = luminance / Double(count)
let blackFraction = Double(black) / Double(count), amberFraction = Double(amber) / Double(count)
let limeFraction = Double(lime) / Double(count), accentFraction = amberFraction + limeFraction
var metrics: [String: Any] = ["width": width, "height": height, "pixels": count,
    "mean_luminance": mean, "near_black_fraction": blackFraction, "amber_fraction": amberFraction,
    "lime_fraction": limeFraction, "accent_fraction": accentFraction]
if quadrants {
    // A small interior patch avoids UI, borders and the encoded counter. The
    // channel median rejects the moving stripe when it crosses a sample.
    var colors = [[Int]]()
    for yFraction in [0.25, 0.75] {
        for xFraction in [0.25, 0.75] {
            let cx = rect.minX + rect.width * xFraction
            let cy = rect.minY + rect.height * yFraction
            let rx = max(1, Int(rect.width * 0.05))
            let ry = max(1, Int(rect.height * 0.05))
            var channels = [[UInt8]](repeating: [], count: 3)
            for y in max(0, Int(cy) - ry)..<min(height, Int(cy) + ry) {
                for x in max(0, Int(cx) - rx)..<min(width, Int(cx) + rx) {
                    let index = (y * width + x) * 4
                    for channel in 0..<3 { channels[channel].append(pixels[index + channel]) }
                }
            }
            colors.append(channels.map { values in
                let sorted = values.sorted()
                return Int(sorted[sorted.count / 2])
            })
        }
    }
    metrics["quadrants_rgb"] = colors
}
let data = try JSONSerialization.data(withJSONObject: metrics, options: [.sortedKeys])
print(String(decoding: data, as: UTF8.self))
if assertPattern && (mean < 20 || blackFraction > 0.8 || amberFraction < 0.001) {
    fail("FAIL: video is black or the amber test pattern is missing")
}
if assertUI && (mean < 2 || blackFraction > 0.98 || accentFraction < 0.0001) {
    fail("FAIL: UI is blank or its accent color is missing")
}
