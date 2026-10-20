// 将用户原图转换为平台资源，保留原始图形与长宽比，不重新绘制标志。
import Foundation
import CoreGraphics
import ImageIO

enum IconError: Error { case invalidSource, bitmap, encoding }
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let icons = root.appendingPathComponent("crates/app/icons")
let sourceURL = icons.appendingPathComponent("source/fesilent-user-icon.png")
guard let source = CGImageSourceCreateWithURL(sourceURL as CFURL, nil),
      let original = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
    throw IconError.invalidSource
}

func bitmap(_ image: CGImage) throws -> [UInt8] {
    var pixels = [UInt8](repeating: 0, count: image.width * image.height * 4)
    let ok = pixels.withUnsafeMutableBytes { buffer -> Bool in
        guard let context = CGContext(
            data: buffer.baseAddress, width: image.width, height: image.height,
            bitsPerComponent: 8, bytesPerRow: image.width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return false }
        context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        return true
    }
    guard ok else { throw IconError.bitmap }
    return pixels
}

func imageFromPixels(_ pixels: [UInt8], width: Int, height: Int) throws -> CGImage {
    guard let provider = CGDataProvider(data: Data(pixels) as CFData),
          let image = CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent
          ) else { throw IconError.bitmap }
    return image
}

func trimTransparent(_ image: CGImage) throws -> CGImage {
    let pixels = try bitmap(image)
    var left = image.width, top = image.height, right = 0, bottom = 0
    for y in 0..<image.height {
        for x in 0..<image.width where pixels[(y * image.width + x) * 4 + 3] > 0 {
            left = min(left, x); right = max(right, x + 1)
            top = min(top, y); bottom = max(bottom, y + 1)
        }
    }
    guard right > left, bottom > top,
          let cropped = image.cropping(to: CGRect(
            x: left, y: top, width: right - left, height: bottom - top
          )) else { throw IconError.invalidSource }
    return cropped
}

func fit(_ image: CGImage, size: Int, fill: CGFloat) throws -> CGImage {
    guard let context = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { throw IconError.bitmap }
    let scale = CGFloat(size) * fill / CGFloat(max(image.width, image.height))
    let width = CGFloat(image.width) * scale, height = CGFloat(image.height) * scale
    context.interpolationQuality = .high
    context.draw(image, in: CGRect(
        x: (CGFloat(size) - width) / 2, y: (CGFloat(size) - height) / 2,
        width: width, height: height
    ))
    guard let result = context.makeImage() else { throw IconError.bitmap }
    return result
}

func png(_ image: CGImage) throws -> Data {
    let data = NSMutableData()
    guard let dest = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil)
    else { throw IconError.encoding }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { throw IconError.encoding }
    return data as Data
}

func save(_ image: CGImage, _ name: String) throws {
    try png(image).write(to: icons.appendingPathComponent(name), options: .atomic)
    print("已生成 \(name)（\(image.width)×\(image.height)）")
}

let cropped = try trimTransparent(original)
// 应用图标保留黑底和白色标志，外沿透明并预留系统图标的安全空间。
let app = try fit(cropped, size: 1024, fill: 0.82)
try save(app, "icon.png")
try save(fit(cropped, size: 32, fill: 0.90), "icon-32.png")
try save(app, "windows/mouseshare-icon.png")

// 模板只保留白色标志的 alpha，避免菜单栏显示一块不透明的黑色底板。
var template = try bitmap(cropped)
for i in stride(from: 0, to: template.count, by: 4) {
    let alpha = Double(template[i + 3])
    let luminance = alpha > 0
        ? (Double(template[i]) * 0.2126 + Double(template[i + 1]) * 0.7152
            + Double(template[i + 2]) * 0.0722) * 255 / alpha : 0
    // 源图外缘含浅色半透明消光像素；模板仅取不透明底板内的白色标志。
    let coverage = alpha >= 250 ? min(1, max(0, (luminance - 100) / 145)) : 0
    let mask = UInt8((alpha * coverage).rounded())
    template[i] = mask; template[i + 1] = mask
    template[i + 2] = mask; template[i + 3] = mask
}
let glyph = try trimTransparent(imageFromPixels(template, width: cropped.width, height: cropped.height))
try save(fit(glyph, size: 44, fill: 0.84), "tray.png")

// 现代 Windows 支持 PNG 编码的多尺寸 ICO；保留现有七档尺寸。
let sizes = [16, 24, 32, 48, 64, 128, 256]
let frames = try sizes.map { try png(fit(cropped, size: $0, fill: 0.90)) }
var ico = Data()
func appendLE(_ value: Int, count: Int) {
    for shift in 0..<count { ico.append(UInt8((value >> (shift * 8)) & 255)) }
}
appendLE(0, count: 2); appendLE(1, count: 2); appendLE(sizes.count, count: 2)
var offset = 6 + sizes.count * 16
for (size, frame) in zip(sizes, frames) {
    ico.append(UInt8(size == 256 ? 0 : size))
    ico.append(UInt8(size == 256 ? 0 : size))
    appendLE(0, count: 2); appendLE(1, count: 2); appendLE(32, count: 2)
    appendLE(frame.count, count: 4); appendLE(offset, count: 4)
    offset += frame.count
}
for frame in frames { ico.append(frame) }
try ico.write(to: icons.appendingPathComponent("windows/mouseshare.ico"), options: .atomic)
print("已生成 Windows ICO（16/24/32/48/64/128/256）")
