//! 封面字节 → `slint::Image` 与点云用的裸像素。
//!
//! 封面 URL 指向音乐平台的 CDN,和播放直链一样会过期 —— 过期后拿到的是
//! HTML 错误页,不是图。所以这里的失败路径是常态路径:解不出来就返回
//! `None`,播放页留在无封面形态,绝不 panic 掉 UI 线程。

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// 送进可视化的封面像素:RGBA8,长边已收进 [`COVER_TEXTURE_SIZE`]。
///
/// 与给 `cover-art` 的那张图是同一次解码的两个出口:界面要 `slint::Image`,
/// 点云要能上传成 GPU 纹理的裸像素(见 `render3d::cloud`)。
pub struct CoverPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 点云采样用的封面纹理边长上限,同原版默认档的 512。
///
/// 点云是 183×183,再高的纹理一个格点也采不到;原图动辄上千像素,原样传上去
/// 只是白搬内存。
pub const COVER_TEXTURE_SIZE: u32 = 512;

/// 把一段图片字节(jpeg/png)解成可设给 `cover-art` 属性的图,外加点云用的像素。
/// 字节不是图时返回 `None` —— 直链过期的 HTML 页、截断的下载都走这条。
pub fn decode(
    bytes: &[u8],
) -> Option<(slint::Image, CoverPixels)> {
    let decoded = image::load_from_memory(bytes).ok()?;
    // 界面那张按原尺寸给,`image-fit: cover` 自己缩;点云那张先收进纹理预算。
    let full = decoded.to_rgba8();
    let (w, h) = full.dimensions();
    let image =
        slint::Image::from_rgba8(SharedPixelBuffer::<
            Rgba8Pixel,
        >::clone_from_slice(
            full.as_raw(), w, h
        ));

    let long_side = w.max(h);
    let shrunk = if long_side > COVER_TEXTURE_SIZE {
        // `thumbnail` 是盒式降采样,比 Lanczos 快一个量级。点云一个格点采一大片,
        // 重采样质量在这里看不出来。
        let scale = f64::from(COVER_TEXTURE_SIZE)
            / f64::from(long_side);
        let target = |side: u32| {
            (f64::from(side) * scale).round().max(1.0)
                as u32
        };
        decoded.thumbnail(target(w), target(h)).to_rgba8()
    } else {
        full
    };
    let (pw, ph) = shrunk.dimensions();

    Some((
        image,
        CoverPixels {
            width: pw,
            height: ph,
            rgba: shrunk.into_raw(),
        },
    ))
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
        let (img, pixels) =
            decode(&png(1, 1)).expect("合法 PNG 应能解码");
        assert_eq!(img.size().width, 1);
        assert_eq!(img.size().height, 1);
        assert_eq!((pixels.width, pixels.height), (1, 1));
    }

    /// 大封面收进纹理预算:长边压到上限、宽高比保住、像素数与宽高对得上。
    /// 上千像素的原图原样搬进 GPU 只是白费内存 —— 点云只有 183×183 个采样点。
    #[test]
    fn decode_shrinks_large_covers_to_the_texture_budget() {
        let (_, pixels) = decode(&png(1200, 800))
            .expect("合法 PNG 应能解码");
        assert_eq!(pixels.width, COVER_TEXTURE_SIZE);
        // 1200:800 = 3:2,512 宽对应 341 高(四舍五入)。
        assert_eq!(pixels.height, 341);
        assert_eq!(
            pixels.rgba.len() as u32,
            pixels.width * pixels.height * 4,
            "像素数与宽高对不上"
        );
    }

    /// 小于预算的封面原样留着,不放大 —— 放大只会糊,一个格点也多不出来。
    #[test]
    fn decode_keeps_small_covers_untouched() {
        let (_, pixels) = decode(&png(300, 300))
            .expect("合法 PNG 应能解码");
        assert_eq!(
            (pixels.width, pixels.height),
            (300, 300)
        );
    }

    /// 在内存里编一张纯色 PNG,免得在测试里贴一段魔法字节。
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 20, 30, 255]),
        )
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("内存里编 PNG 不该失败");
        out.into_inner()
    }
}
