//! 把一首歌的字节下到本机。
//!
//! 与其余端点不同,这一条**不把响应攒在内存里**:一首无损转出来的 mp3 是几十
//! MB,攒完再交出去等于在手机上凭空要一块同样大的堆,而落盘那一侧本来就是流式的。
//! 所以调用方给一个写入口,字节边收边写。
//!
//! 写到哪里是平台的事(安卓进系统音乐目录、桌面进 `~/Music`),这一层只管
//! 「从服务端到那个写入口」这一段。

use crate::ApiError;
use crate::url::download_url;

/// `GET /download/{track_id}` —— 整首歌的 mp3 字节,边收边写进 `sink`。
///
/// 返回 `Ok` 才算收全了。中途失败时 `sink` 里已经有半截内容,**由调用方负责
/// 丢掉它** —— 服务端那一侧一旦发出 200 就没有回头改状态码的余地,截断的响应
/// 与正常的响应在 HTTP 上长得一样(见 `server/src/routes/play/download.rs`)。
///
/// `progress` 收到的是 (已收字节, 总字节)。总数是 `Option` 而不是 `u64`:
/// 服务端转码那一路事前算不出会出多少字节,只能不给 —— 签成 `u64` 的话那条路
/// 只能填一个猜的数,而界面会拿它画一条走到一半就跳的进度条,比没有进度更糟。
///
/// 试听片段会以 `ApiError::Server { code: contract::TRIAL_ONLY, .. }` 拒绝。
pub async fn download(
    track_id: &str,
    sink: impl std::io::Write + Send + 'static,
    progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> Result<(), ApiError> {
    crate::platform::download(
        download_url(track_id),
        sink,
        progress,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, Write as _};
    use std::sync::{Arc, Mutex};

    /// 在随机端口上答一次 `body`,然后关掉。
    ///
    /// 手写而不是拉一个 HTTP 服务端进 dev-dependency:要的只是"一条真的
    /// TCP 连接上来一份带 Content-Length 的响应",而那是十行。
    fn serve_once(body: Vec<u8>) -> String {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0")
                .expect("绑不上回环端口");
        let addr =
            listener.local_addr().expect("取不到端口");

        std::thread::spawn(move || {
            let (stream, _) =
                listener.accept().expect("没人连上来");
            let mut reader =
                std::io::BufReader::new(&stream);
            // 请求头读到空行为止 —— 不读完的话,写响应时对端可能还在发。
            loop {
                let mut line = String::new();
                if reader
                    .read_line(&mut line)
                    .expect("读不动请求")
                    == 0
                    || line == "\r\n"
                {
                    break;
                }
            }

            let mut stream = &stream;
            let head = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: audio/mpeg\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });

        format!("http://{addr}/download/x")
    }

    /// 一次完整的下载:字节一个不少地进 sink,进度最后停在总数上。
    ///
    /// 进度还必须**稀疏**:每个 chunk 都报的话一首歌要发出去几千次,
    /// 而每一次都要跨回事件循环才敢碰界面状态。
    #[tokio::test]
    async fn bytes_land_in_the_sink_and_progress_ends_at_the_total()
     {
        const SIZE: usize = 3 * 1024 * 1024;
        let body: Vec<u8> =
            (0..SIZE).map(|i| i as u8).collect();
        let url = serve_once(body.clone());

        let sink = Arc::new(Mutex::new(Vec::new()));
        let reports = Arc::new(Mutex::new(Vec::new()));

        let written = Arc::clone(&sink);
        let seen = Arc::clone(&reports);
        crate::platform::download(
            url,
            Sink(written),
            move |done, total| {
                seen.lock()
                    .expect("记进度的锁被毒化了")
                    .push((done, total));
            },
        )
        .await
        .expect("这一次下载不该失败");

        assert_eq!(
            *sink.lock().expect("sink 的锁被毒化了"),
            body,
            "写进 sink 的字节与服务端发的不一致"
        );

        let reports =
            reports.lock().expect("记进度的锁被毒化了");
        assert_eq!(
            reports.last().copied(),
            Some((SIZE as u64, Some(SIZE as u64))),
            "最后一次进度必须停在总数上,否则进度条永远差最后一截"
        );
        assert!(
            reports.len() < 64,
            "进度报得太密了({} 次)—— 每一次都要跨回事件循环",
            reports.len()
        );
    }

    /// 一个把字节攒进共享缓冲的写入口。
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(
            &mut self,
            buf: &[u8],
        ) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("sink 的锁被毒化了")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
