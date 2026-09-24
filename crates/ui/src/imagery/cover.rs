//! 封面字节 → `slint::Image` 与点云用的裸像素。
//!
//! 封面 URL 指向音乐平台的 CDN,和播放直链一样会过期 —— 过期后拿到的是
//! HTML 错误页,不是图。所以这里的失败路径是常态路径:解不出来就返回
//! `None`,播放页留在无封面形态,绝不 panic 掉 UI 线程。

use slint::{Rgba8Pixel, SharedPixelBuffer};

pub use crate::viz::CoverPixels;

/// 点云采样用的封面纹理边长上限,同原版默认档的 512。
///
/// 点云是 96×96,再高的纹理一个格点也采不到;原图动辄上千像素,原样传上去
/// 只是白搬内存。
pub const COVER_TEXTURE_SIZE: u32 = 512;

/// 点云那张:长边收进 [`COVER_TEXTURE_SIZE`],本来就小的原样留着。
fn cover_pixels(
    decoded: &image::DynamicImage,
    full: &image::RgbaImage,
) -> CoverPixels {
    let (w, h) = full.dimensions();
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
        full.clone()
    };
    let (pw, ph) = shrunk.dimensions();
    CoverPixels {
        width: pw,
        height: ph,
        rgba: shrunk.into_raw(),
    }
}

/// 后台解出来的一张封面:原尺寸像素(UI 线程包成 `slint::Image`)、
/// 点云用的缩小像素、极光用的三个主色。全都能过线程。
pub struct DecodedCover {
    pub full: SharedPixelBuffer<Rgba8Pixel>,
    pub pixels: CoverPixels,
    pub colors: Option<[[u8; 3]; 3]>,
}

/// 同时在解的封面数上限的那道门。
///
/// 连按下一首时每一首都要解一张兆级的图;不设上限的话一串解码同时占满
/// 后台线程,真正要看的那张反而排在后面。
pub struct DecodeGate {
    free: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
}

/// 占着的一个名额,丢掉就还回去。
pub struct Slot<'a>(&'a DecodeGate);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut free = self
            .0
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *free += 1;
        self.0.freed.notify_one();
    }
}

impl DecodeGate {
    pub const fn new(slots: usize) -> Self {
        Self {
            free: std::sync::Mutex::new(slots),
            freed: std::sync::Condvar::new(),
        }
    }

    /// 等到有空名额再进去。只在后台线程上调 —— 它会阻塞。
    pub fn enter(&self) -> Slot<'_> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self
                .freed
                .wait(free)
                .unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Slot(self)
    }
}

/// 封面同时最多解几张。当前这首只有一张,留一个给「上一首还没解完」。
static DECODING: DecodeGate = DecodeGate::new(2);

/// 在后台线程上解一张封面(#137 ⑥),连同极光要的三个主色。
///
/// `wanted` 在拿到名额之后问一次:排队这段时间里用户可能已经切走了,那就
/// 不白解。回到 UI 线程之后调用方仍要再校验一次身份 —— 解码期间也可能切走。
pub async fn decode_off_thread(
    bytes: Vec<u8>,
    wanted: impl Fn() -> bool + Send + 'static,
) -> Option<DecodedCover> {
    api::off_thread(move || {
        let _slot = DECODING.enter();
        if !wanted() {
            return None;
        }
        decode_detached(&bytes)
    })
    .await
    .flatten()
}

/// 把一段图片字节(jpeg/png)解成原尺寸像素、点云像素与主色。字节不是图时
/// 返回 `None` —— 直链过期的 HTML 页、截断的下载都走这条。不碰 `slint::Image`,
/// 所以能在后台线程上跑。
fn decode_detached(bytes: &[u8]) -> Option<DecodedCover> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let full = decoded.to_rgba8();
    let (w, h) = full.dimensions();
    let pixels = cover_pixels(&decoded, &full);
    let colors = crate::shader::aurora::colors_of(&pixels);
    Some(DecodedCover {
        full: SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            full.as_raw(),
            w,
            h,
        ),
        pixels,
        colors,
    })
}

/// 列表行里那张缩略图的边长上限。
///
/// 行里画的是 40px 逻辑尺寸,2 倍 HiDPI 屏上是 80 物理像素,取 96 留一点余量。
/// 原尺寸解码在这里是不能选的:歌单详情能到近千行,一张 500×500 解出来 1MB,
/// 全摆上就是 GB 级常驻。
pub const THUMBNAIL_SIZE: u32 = 96;

/// 列表行缩略图解出来的像素,还没变成 `slint::Image`。
///
/// 分这一步是为了过线程:`slint::Image` 只能待在 UI 线程上,像素缓冲可以跨。
/// 解码在后台做,UI 线程只剩 [`slint::Image::from_rgba8`] 那一下包装。
pub type ThumbnailPixels = SharedPixelBuffer<Rgba8Pixel>;

/// 把封面字节解成列表行用的缩略图。
///
/// 与 [`decode`] 的差别只在尺寸和不出点云:那一个供播放页那张大图,要原分辨率;
/// 这一个一次要出几十上百张,只能出小的。失败路径同样是常态路径 ——
/// CDN 过期后回的是 HTML 错误页。
pub fn decode_thumbnail(bytes: &[u8]) -> Option<Thumbnail> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let (w, h) = (decoded.width(), decoded.height());

    let long_side = w.max(h);
    let small = if long_side > THUMBNAIL_SIZE {
        // 与点云那一路同一个理由用 `thumbnail`:盒式降采样,快一个量级,
        // 而 96px 上看不出重采样质量的差别。
        let scale = f64::from(THUMBNAIL_SIZE)
            / f64::from(long_side);
        let target = |side: u32| {
            (f64::from(side) * scale).round().max(1.0)
                as u32
        };
        decoded.thumbnail(target(w), target(h)).to_rgba8()
    } else {
        // 比预算还小的原样留着 —— 放大只会糊,一个像素也多不出来。
        decoded.to_rgba8()
    };

    let (tw, th) = small.dimensions();
    Some(Thumbnail {
        pixels: ThumbnailPixels::clone_from_slice(
            small.as_raw(),
            tw,
            th,
        ),
        shrunk: long_side > THUMBNAIL_SIZE,
    })
}

/// [`decode_thumbnail`] 的结果。
pub struct Thumbnail {
    pub pixels: ThumbnailPixels,
    /// 源图比预算大、被缩过。
    ///
    /// 磁盘缓存里读出来的若是这种,那是改存缩略图之前落的原图(#117),
    /// 该换成缩好的那份 —— 否则每次启动都要再解一遍原图。
    pub shrunk: bool,
}

/// 缩略图编成 PNG,落盘用。
///
/// 存缩好的而不是网上取回来的原图:一张 96px 的 PNG 解起来比上千像素的
/// JPEG 快一个量级,而磁盘缓存每次启动都要整批重解(内存那层是空的)。
pub fn encode_png(
    pixels: &ThumbnailPixels,
) -> Option<Vec<u8>> {
    let image = image::RgbaImage::from_raw(
        pixels.width(),
        pixels.height(),
        pixels.as_bytes().to_vec(),
    )?;
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

/// 磁盘缓存里那一份解成缩略图。存的若还是改前落的原图,顺手给出该换上去的
/// 那份(第二项),调用方写回去。
///
/// 这一组都在后台线程上跑,所以只碰字节与像素。
pub fn from_disk(
    bytes: &[u8],
) -> Option<(ThumbnailPixels, Option<Vec<u8>>)> {
    let thumb = decode_thumbnail(bytes)?;
    let rewrite = thumb
        .shrunk
        .then(|| encode_png(&thumb.pixels))
        .flatten();
    Some((thumb.pixels, rewrite))
}

/// 网上取回来的原图解成缩略图,外加该落盘的那一份(第二项)—— 缩好的,不是原图。
pub fn from_network(
    bytes: &[u8],
) -> Option<(ThumbnailPixels, Option<Vec<u8>>)> {
    let thumb = decode_thumbnail(bytes)?;
    let store = encode_png(&thumb.pixels);
    Some((thumb.pixels, store))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 直链过期返回 HTML 错误页:解码必须失败为 None,不得 panic 掉 UI 线程。
    #[test]
    fn rejects_html_error_page() {
        let html =
            b"<html><body>403 Forbidden</body></html>";
        assert!(decode_detached(html).is_none());
    }

    /// 最小合法 PNG 解出 1×1 图:bytes → 像素缓冲 → slint::Image 全链可用。
    #[test]
    fn decodes_minimal_png() {
        let decoded = decode_detached(&png(1, 1))
            .expect("合法 PNG 应能解码");
        let img = slint::Image::from_rgba8(decoded.full);
        let pixels = decoded.pixels;
        assert_eq!(img.size().width, 1);
        assert_eq!(img.size().height, 1);
        assert_eq!((pixels.width, pixels.height), (1, 1));
    }

    /// 大封面收进纹理预算:长边压到上限、宽高比保住、像素数与宽高对得上。
    /// 上千像素的原图原样搬进 GPU 只是白费内存 —— 点云只有 183×183 个采样点。
    #[test]
    fn decode_shrinks_large_covers_to_the_texture_budget() {
        let pixels = decode_detached(&png(1200, 800))
            .map(|d| d.pixels)
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
        let pixels = decode_detached(&png(300, 300))
            .map(|d| d.pixels)
            .expect("合法 PNG 应能解码");
        assert_eq!(
            (pixels.width, pixels.height),
            (300, 300)
        );
    }

    /// 大封面解成缩略图:长边压到预算、宽高比保住。
    ///
    /// 不压的话歌单详情近千行会把原图整批搬进内存 —— 那是 GB 级。
    #[test]
    fn decode_thumbnail_fits_the_thumbnail_budget() {
        let img = decode_thumbnail(&png(1200, 800))
            .expect("合法 PNG 应能解码")
            .pixels;
        assert_eq!(img.width(), THUMBNAIL_SIZE);
        // 1200:800 = 3:2,96 宽对应 64 高
        assert_eq!(img.height(), 64);
    }

    /// 小于预算的封面原样留着,不放大 —— 与 `decode` 同一条规矩。
    #[test]
    fn decode_thumbnail_keeps_small_covers_untouched() {
        let img = decode_thumbnail(&png(48, 48))
            .expect("合法 PNG 应能解码")
            .pixels;
        assert_eq!((img.width(), img.height()), (48, 48));
    }

    /// 直链过期回的 HTML 错误页解不出图,返回 None 而不是 panic 掉 UI 线程。
    #[test]
    fn decode_thumbnail_rejects_a_html_error_page() {
        let html =
            b"<html><body>403 Forbidden</body></html>";
        assert!(decode_thumbnail(html).is_none());
    }

    // ── 磁盘缓存里的那一份([`from_network`] 与 [`from_disk`])──

    /// 一段字节解出来的边长。
    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        let decoded = image::load_from_memory(bytes)
            .expect("落盘的那份该是一张图");
        (decoded.width(), decoded.height())
    }

    /// 网上取回来的大图,落盘的是缩好的那份,不是原图。
    ///
    /// 存原图的话,每次启动内存那层是空的,磁盘上整批原图要再解一遍 ——
    /// 进每日推荐那一下的卡就是这么来的。
    #[test]
    fn a_fetched_cover_is_stored_as_a_thumbnail() {
        let (pixels, store) = from_network(&png(1200, 800))
            .expect("合法 PNG 该解得出来");

        let stored = store.expect("解得出来就该有一份落盘");
        assert_eq!(dimensions(&stored), (96, 64));
        assert_eq!(
            (pixels.width(), pixels.height()),
            (96, 64)
        );
    }

    /// 网上回来的不是图(CDN 过期的 HTML 页):什么都不落盘。
    #[test]
    fn a_fetched_error_page_is_not_stored() {
        assert!(
            from_network(b"<html>403</html>").is_none()
        );
    }

    /// 磁盘上还是改之前落的原图:解出来的同时给出缩好的那份,换上去。
    ///
    /// 不换的话,老用户的缓存目录里全是原图,改存缩略图对他们一张都不生效,
    /// 直到 64MB 的上限把它们挤出去。
    #[test]
    fn a_legacy_original_on_disk_is_rewritten_as_a_thumbnail()
     {
        let (pixels, rewrite) = from_disk(&png(1200, 800))
            .expect("合法 PNG 该解得出来");

        let rewrite = rewrite.expect("旧的原图该被换掉");
        assert_eq!(dimensions(&rewrite), (96, 64));
        assert_eq!(
            (pixels.width(), pixels.height()),
            (96, 64)
        );
    }

    /// 磁盘上已经是缩略图:原样用,不再写一遍。
    ///
    /// 每次命中都写的话,滚一次列表就是几十次写盘,还会把 mtime 刷新到
    /// 淘汰顺序失真。
    #[test]
    fn a_stored_thumbnail_is_not_rewritten() {
        let (_, store) = from_network(&png(1200, 800))
            .expect("合法 PNG 该解得出来");
        let stored = store.expect("解得出来就该有一份落盘");

        let (pixels, rewrite) = from_disk(&stored)
            .expect("存下的那份该解得出来");

        assert!(
            rewrite.is_none(),
            "已经是缩略图了,不该再写"
        );
        assert_eq!(
            (pixels.width(), pixels.height()),
            (96, 64)
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

    // ── 离开 UI 线程解码(#137 ⑥)──

    /// 忙等着把一个 future 跑完。解码在后台线程上,这里只管轮询到它回来。
    fn block_on<F: core::future::Future>(
        future: F,
    ) -> F::Output {
        use core::task::{Context, Poll};

        let mut cx =
            Context::from_waker(core::task::Waker::noop());
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(value) =
                future.as_mut().poll(&mut cx)
            {
                return value;
            }
            std::thread::yield_now();
        }
    }

    /// 后台解出来的是能过线程的像素与主色,`slint::Image` 留给 UI 线程包一下。
    #[test]
    fn a_cover_decodes_off_the_ui_thread() {
        fn must_cross_threads<T: Send>(_: &T) {}

        let caller = std::thread::current().id();
        let ran_on = std::sync::Arc::new(
            std::sync::Mutex::new(None),
        );
        let probe = ran_on.clone();
        let decoded = block_on(decode_off_thread(
            png(8, 8),
            move || {
                *probe.lock().unwrap() =
                    Some(std::thread::current().id());
                true
            },
        ))
        .expect("合法 PNG 应能解码");

        must_cross_threads(&decoded);
        assert_eq!(
            (decoded.pixels.width, decoded.pixels.height),
            (8, 8)
        );
        assert_ne!(
            *ran_on.lock().unwrap(),
            Some(caller),
            "解码还在调用方线程上跑"
        );
    }

    /// 轮到它解码时已经不是当前这首了:直接放弃,不白解一张兆级的图。
    #[test]
    fn a_cover_no_longer_wanted_is_not_decoded() {
        assert!(
            block_on(decode_off_thread(png(8, 8), || {
                false
            }))
            .is_none()
        );
    }

    /// 同时在解的封面有上限:第二个要等第一个让出名额。
    #[test]
    fn decoding_waits_for_a_free_slot() {
        let gate = std::sync::Arc::new(DecodeGate::new(1));
        let held = gate.enter();

        let entered = std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        );
        let waiting = {
            let (gate, entered) =
                (gate.clone(), entered.clone());
            std::thread::spawn(move || {
                let _slot = gate.enter();
                entered.store(
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                );
            })
        };

        std::thread::sleep(
            std::time::Duration::from_millis(50),
        );
        assert!(
            !entered
                .load(std::sync::atomic::Ordering::SeqCst),
            "名额满了还是进去了"
        );
        drop(held);
        waiting.join().unwrap();
        assert!(
            entered
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }
}
