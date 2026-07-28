//! 封面字节 → `slint::Image`。
//!
//! 封面 URL 指向音乐平台的 CDN,和播放直链一样会过期 —— 过期后拿到的是
//! HTML 错误页,不是图。所以这里的失败路径是常态路径:解不出来就返回
//! `None`,播放页留在无封面形态,绝不 panic 掉 UI 线程。

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// 把一段图片字节(jpeg/png)解成可设给 `cover-art` 属性的图。
/// 字节不是图时返回 `None` —— 直链过期的 HTML 页、截断的下载都走这条。
pub fn decode(bytes: &[u8]) -> Option<slint::Image> {
    let decoded =
        image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            decoded.as_raw(),
            w,
            h,
        );
    Some(slint::Image::from_rgba8(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 直链过期返回 HTML 错误页:解码必须失败为 None,不得 panic 掉 UI 线程。
    #[test]
    fn rejects_html_error_page() {
        let html =
            b"<html><body>403 Forbidden</body></html>";
        assert!(decode(html).is_none());
    }

    /// 最小合法 PNG 解出 1×1 图:bytes → 像素缓冲 → slint::Image 全链可用。
    #[test]
    fn decodes_minimal_png() {
        // 用 image 自己编一张 1×1 PNG,免得在测试里贴一段魔法字节。
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([10, 20, 30, 255]),
        )
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("内存里编 1×1 PNG 不该失败");

        let img = decode(png.get_ref())
            .expect("合法 PNG 应能解码");
        assert_eq!(img.size().width, 1);
        assert_eq!(img.size().height, 1);
    }
}
