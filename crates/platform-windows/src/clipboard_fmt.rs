//! Windows 剪贴板格式 ↔ 中立格式转换（DEVELOPMENT.md §5.2，TECH-REVIEW B-6）。
//!
//! 纯逻辑（不碰 Win32 API），任何平台可单测。协议层统一传：
//! - HTML：纯 fragment（UTF-8）。Windows 的 `CF_HTML` 带 Version/StartHTML/...
//!   头部，本模块负责封装/剥离，**偏移按 UTF-8 字节计**（含中文多字节）。
//! - 图片：PNG。Windows 的 `CF_DIB`(32bpp) 与 PNG 双向转码（注意 BGRA↔RGBA、
//!   行序 biHeight 正负）。

// ───────────────────────── CF_HTML ─────────────────────────

const HTML_OPEN: &str = "<html>\r\n<body>\r\n<!--StartFragment-->";
const HTML_CLOSE: &str = "<!--EndFragment-->\r\n</body>\r\n</html>";

/// 把纯 HTML fragment 封装为 CF_HTML（带头部，偏移为 UTF-8 字节）。
pub fn cf_html_wrap(fragment: &str) -> Vec<u8> {
    // 头部各偏移字段固定 10 位十进制，使头部长度恒定，便于一次算出偏移。
    let header_template = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let header_len = header_template.len();

    let start_html = header_len;
    let start_fragment = start_html + HTML_OPEN.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + HTML_CLOSE.len();

    let header = format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    );
    debug_assert_eq!(header.len(), header_len, "头部长度必须恒定");

    let mut out = Vec::with_capacity(end_html);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(HTML_OPEN.as_bytes());
    out.extend_from_slice(fragment.as_bytes());
    out.extend_from_slice(HTML_CLOSE.as_bytes());
    out
}

/// 从 CF_HTML 剥离出纯 HTML fragment（按 StartFragment/EndFragment 字节偏移）。
pub fn cf_html_strip(cf: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(cf).ok()?;
    let start = parse_offset(text, "StartFragment:")?;
    let end = parse_offset(text, "EndFragment:")?;
    if start > end || end > cf.len() {
        return None;
    }
    String::from_utf8(cf[start..end].to_vec()).ok()
}

fn parse_offset(text: &str, key: &str) -> Option<usize> {
    let idx = text.find(key)? + key.len();
    let rest = &text[idx..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// ───────────────────────── CF_DIB ↔ PNG ─────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ImgError {
    #[error("DIB 头部不完整或不支持: {0}")]
    Dib(String),
    #[error("PNG 编解码失败: {0}")]
    Png(String),
}

/// 解析 32bpp CF_DIB → PNG 字节。支持 40 字节(BITMAPINFOHEADER)或 124 字节(V5)头，
/// 按 biHeight 正负处理自下而上/自上而下行序，BGRA→RGBA。
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, ImgError> {
    if dib.len() < 4 {
        return Err(ImgError::Dib("过短".into()));
    }
    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    if header_size < 40 || dib.len() < header_size {
        return Err(ImgError::Dib(format!("头部大小 {header_size} 不支持")));
    }
    let width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
    let height_raw = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]);
    if bit_count != 32 {
        return Err(ImgError::Dib(format!("仅支持 32bpp，得到 {bit_count}")));
    }
    let w = width.unsigned_abs() as usize;
    let h = height_raw.unsigned_abs() as usize;
    let top_down = height_raw < 0;
    let stride = w * 4;
    let pixels = &dib[header_size..];
    if pixels.len() < stride * h {
        return Err(ImgError::Dib("像素数据不足".into()));
    }

    // 转成自上而下的 RGBA
    let mut rgba = vec![0u8; stride * h];
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let src = &pixels[src_row * stride..src_row * stride + stride];
        let dst = &mut rgba[row * stride..row * stride + stride];
        for px in 0..w {
            // DIB: BGRA → PNG: RGBA
            dst[px * 4] = src[px * 4 + 2]; // R
            dst[px * 4 + 1] = src[px * 4 + 1]; // G
            dst[px * 4 + 2] = src[px * 4]; // B
            dst[px * 4 + 3] = src[px * 4 + 3]; // A
        }
    }

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| ImgError::Png(e.to_string()))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| ImgError::Png(e.to_string()))?;
    }
    Ok(out)
}

/// PNG 字节 → 32bpp CF_DIB（BITMAPINFOHEADER 40 字节 + 自下而上 BGRA）。
pub fn png_to_dib(png_bytes: &[u8]) -> Result<Vec<u8>, ImgError> {
    let dec = png::Decoder::new(png_bytes);
    let mut reader = dec.read_info().map_err(|e| ImgError::Png(e.to_string()))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| ImgError::Png(e.to_string()))?;
    let w = info.width as usize;
    let h = info.height as usize;

    // 统一取出 RGBA
    let rgba = to_rgba(&buf[..info.buffer_size()], info.color_type, w, h)?;

    let stride = w * 4;
    let mut out = Vec::with_capacity(40 + stride * h);
    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight (>0 自下而上)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression BI_RGB
    out.extend_from_slice(&((stride * h) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    // 像素：自下而上，RGBA→BGRA
    for row in 0..h {
        let src_row = h - 1 - row;
        let src = &rgba[src_row * stride..src_row * stride + stride];
        for px in 0..w {
            out.push(src[px * 4 + 2]); // B
            out.push(src[px * 4 + 1]); // G
            out.push(src[px * 4]); // R
            out.push(src[px * 4 + 3]); // A
        }
    }
    Ok(out)
}

fn to_rgba(data: &[u8], ct: png::ColorType, w: usize, h: usize) -> Result<Vec<u8>, ImgError> {
    let n = w * h;
    match ct {
        png::ColorType::Rgba => Ok(data.to_vec()),
        png::ColorType::Rgb => {
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                out[i * 4] = data[i * 3];
                out[i * 4 + 1] = data[i * 3 + 1];
                out[i * 4 + 2] = data[i * 3 + 2];
                out[i * 4 + 3] = 255;
            }
            Ok(out)
        }
        other => Err(ImgError::Png(format!("暂不支持的 PNG 颜色类型 {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn cf_html_往返英文() {
        let frag = "<b>Hello</b> world";
        let cf = cf_html_wrap(frag);
        assert_eq!(cf_html_strip(&cf).as_deref(), Some(frag));
    }

    #[test]
    fn cf_html_往返中文多字节偏移正确() {
        // 中文每字 3 字节，验证 StartFragment/EndFragment 按字节偏移正确（B-6）
        let frag = "<p>你好，世界！<b>测试</b></p>";
        let cf = cf_html_wrap(frag);
        // 头部声明的偏移必须按字节切出原 fragment
        assert_eq!(cf_html_strip(&cf).as_deref(), Some(frag));
        // 确认 EndFragment - StartFragment 等于 fragment 的字节数
        let text = std::str::from_utf8(&cf).unwrap();
        let sf = parse_offset(text, "StartFragment:").unwrap();
        let ef = parse_offset(text, "EndFragment:").unwrap();
        assert_eq!(ef - sf, frag.len(), "字节偏移差应等于 UTF-8 字节长度");
    }

    #[test]
    fn cf_html_头部长度恒定() {
        // 不同长度 fragment 头部长度应一致（10 位定宽偏移）
        let a = cf_html_wrap("x");
        let b = cf_html_wrap("很长很长很长很长很长很长的内容");
        let ha = std::str::from_utf8(&a).unwrap().find("<html>").unwrap();
        let hb = std::str::from_utf8(&b).unwrap().find("<html>").unwrap();
        assert_eq!(ha, hb);
    }

    /// 构造一个 2x2 自下而上 BGRA DIB，转 PNG 再解回，校验像素与颜色通道。
    #[test]
    fn dib自下而上转png像素正确() {
        let w = 2i32;
        let h = 2i32;
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&w.to_le_bytes());
        dib.extend_from_slice(&h.to_le_bytes()); // 正 = 自下而上
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]); // 其余头部字段
        // 像素（自下而上）：底行 = 红、绿；顶行 = 蓝、白。BGRA 序
        // 底行
        dib.extend_from_slice(&[0, 0, 255, 255]); // 红 (B0 G0 R255)
        dib.extend_from_slice(&[0, 255, 0, 255]); // 绿
        // 顶行
        dib.extend_from_slice(&[255, 0, 0, 255]); // 蓝
        dib.extend_from_slice(&[255, 255, 255, 255]); // 白

        let png = dib_to_png(&dib).unwrap();
        // 解回校验顶左像素应为蓝(0,0,255)
        let dec = png::Decoder::new(&png[..]);
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        // PNG 自上而下，第一个像素 = 原 DIB 顶左 = 蓝
        assert_eq!(&buf[0..4], &[0, 0, 255, 255], "顶左应为蓝(RGBA)");
    }

    #[test]
    fn png_dib_往返像素一致() {
        // 生成一张 3x2 RGBA PNG
        let w = 3u32;
        let h = 2u32;
        let mut orig = Vec::new();
        for i in 0..(w * h) as u8 {
            orig.extend_from_slice(&[i * 10, i * 20, i * 30, 255]);
        }
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&orig).unwrap();
        }
        // PNG → DIB → PNG，像素应一致
        let dib = png_to_dib(&png_bytes).unwrap();
        let png2 = dib_to_png(&dib).unwrap();
        let dec = png::Decoder::new(&png2[..]);
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf, orig, "PNG→DIB→PNG 往返像素必须一致");
    }
}
