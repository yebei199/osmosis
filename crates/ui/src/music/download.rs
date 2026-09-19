//! 把一首歌下到本机,统一 mp3。
//!
//! 三段:**落点**由平台给(安卓进系统音乐目录,桌面那一份下一期),**字节**
//! 由 `api::download` 流式送来,**说给用户听的话**分两种 —— 进行中那句是
//! 投影(`Shell.download-text`,唯一写者是本模块),完成与失败那两句是一次性
//! 提示,走 [`crate::notice`]。两者的分法见那个模块开头。
//!
//! 「收全了才提交」是这条路唯一的完整性保证:服务端一旦发出 200 就没有回头改
//! 状态码的余地,截断的响应与正常的响应在 HTTP 上长得一样。所以落点必须先写进
//! 一个**待定**的条目,`api::download` 返回 `Ok` 才让它对系统可见,否则丢掉 ——
//! 不这么做的话,断网会在音乐库里留下一首放到一半就停的歌。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::OnceLock;

use super::*;
use crate::Shell;

/// 下载的落点。由平台入口注入(见 `crate::install_download_store`)——
/// 安卓那一份走 MediaStore,住在 `apps/android`。
///
/// 与系统媒体控件同一个接缝形状(`docs/adr/0020`):trait 在这里,
/// 实现在平台入口,ui 不认识 JNI 也不认识 `~/Music`。
pub trait DownloadStore: Send + Sync + 'static {
    /// 开一个**待定**的条目:交出写入口,以及收尾的那一半。
    ///
    /// 分成两半是因为写入口要交给 `api::download` 拿走(它按值收),
    /// 而提交必须发生在写完之后 —— 攥在同一个对象上就拿不回来了。
    fn open(
        &self,
        file_name: &str,
    ) -> std::io::Result<(
        Box<dyn std::io::Write + Send>,
        Box<dyn DownloadCommit>,
    )>;

    /// 存到哪儿了,如「音乐/osmosis」。完成那句话要说清楚文件去了哪 ——
    /// 不说的话用户得自己在文件管理器里找。
    fn location(&self) -> String;
}

/// 一个待定条目的收尾。二选一,不调就等同于 [`Self::discard`]。
pub trait DownloadCommit: Send + 'static {
    /// 收全了:让这个文件对系统(音乐库、文件管理器)可见。
    fn commit(self: Box<Self>) -> std::io::Result<()>;
    /// 没收全:把半截文件去掉。
    fn discard(self: Box<Self>);
}

/// 平台注入的落点。
///
/// `OnceLock` 而不是参数:传参数要给 `run` / `run_with_renderers` 各加一个形参,
/// 而四个平台入口里只有安卓给得出实现,另外三个只能传 `None` —— 为一个端改四处
/// 签名。同 `apps/android/src/controls.rs` 里那条命令通道的理由。
static STORE: OnceLock<Box<dyn DownloadStore>> =
    OnceLock::new();

/// 接上落点。平台入口在 `run*` 之前调一次;不调就是这一端不支持下载。
pub fn install_download_store(
    store: Box<dyn DownloadStore>,
) {
    if STORE.set(store).is_err() {
        log::warn!("下载落点被接了第二次,后一次没有生效");
    }
}

/// 接「下载这一首」。
pub(super) fn bind_download(ui: &MainWindow, deck: &Deck) {
    let weak = ui.as_weak();
    let deck = deck.clone();
    // 在飞的那几首。同一首连点两下会开出两个待定条目,音乐库里于是有两个
    // 同名文件 —— 而第二个多半还是半截的。
    let busy: Rc<RefCell<HashSet<String>>> =
        Rc::new(RefCell::new(HashSet::new()));

    ui.global::<crate::Library>().on_download_track(
        move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            start(&ui, &deck, &busy, id);
        },
    );
}

/// 起一次下载。所有"起不来"的理由在这里就说清楚,不进异步那一段。
fn start(
    ui: &MainWindow,
    deck: &Deck,
    busy: &Rc<RefCell<HashSet<String>>>,
    id: String,
) {
    let Some(store) = STORE.get() else {
        crate::notice::show(
            ui,
            "这一端还不支持下载".to_owned(),
        );
        return;
    };

    if !busy.borrow_mut().insert(id.clone()) {
        crate::notice::show(
            ui,
            "这一首已经在下了".to_owned(),
        );
        return;
    }

    let (artists, title) = describe_track(deck, &id);
    let file_name =
        api::download_file_name(&artists, &title);

    let (writer, commit) = match store.open(&file_name) {
        Ok(opened) => opened,
        Err(err) => {
            busy.borrow_mut().remove(&id);
            crate::notice::show(
                ui,
                format!("存不下来: {err}"),
            );
            return;
        }
    };

    let weak = ui.as_weak();
    ui.global::<Shell>().set_download_text(
        describe_progress(0, None).into(),
    );

    let reporting = ui.as_weak();
    let progress = move |done: u64, total: Option<u64>| {
        let text = describe_progress(done, total);
        // 回调跑在 tokio 的工作线程上 —— 界面状态只有事件循环碰得。
        let _ = reporting.upgrade_in_event_loop(
            move |ui: MainWindow| {
                ui.global::<Shell>()
                    .set_download_text(text.into());
            },
        );
    };

    let busy = Rc::clone(busy);
    let location = store.location();
    slint::spawn_local(async move {
        let result =
            api::download(&id, writer, progress).await;
        // 提交与丢弃都可能出错,而那一样是"没存下来"。
        let outcome = match result {
            Ok(()) => commit
                .commit()
                .map(|()| format!("已存到 {location}"))
                .map_err(|err| format!("存不下来: {err}")),
            Err(err) => {
                commit.discard();
                Err(describe_failure(&err))
            }
        };

        busy.borrow_mut().remove(&id);
        let Some(ui) = weak.upgrade() else { return };
        ui.global::<Shell>()
            .set_download_text(slint::SharedString::new());
        crate::notice::show(
            &ui,
            match outcome {
                Ok(done) => done,
                Err(why) => why,
            },
        );
    })
    .expect("event loop must be running");
}

/// 这一首叫什么。先找列表里那一批的权威副本,再找此刻装着的那一首 ——
/// 播放页上点下载时,那一首未必还在列表里(换过分区、进过别的歌单)。
fn describe_track(
    deck: &Deck,
    id: &str,
) -> (Vec<String>, String) {
    if let Some(track) = deck
        .tracks
        .borrow()
        .iter()
        .find(|track| track.id == id)
    {
        return (
            track.artists.clone(),
            track.title.clone(),
        );
    }

    if let app_core::PlaybackState::Loading(track)
    | app_core::PlaybackState::Playing(track) =
        deck.playback.borrow().state()
        && track.id == id
    {
        return (
            track.artists.clone(),
            track.title.clone(),
        );
    }

    // 两处都没有:名字退回曲目 id,歌照样下得下来。
    (Vec::new(), id.to_owned())
}

/// 进行中那一句。
///
/// 总数不知道时(服务端转码那一路事前算不出会出多少字节)报已收的量,
/// **不报一个猜的百分比** —— 一条走到 80% 就停住不动的进度条,比没有进度更像坏了。
pub(in crate::music) fn describe_progress(
    done: u64,
    total: Option<u64>,
) -> String {
    match total {
        Some(total) if total > 0 => {
            let percent = done.saturating_mul(100) / total;
            format!("下载中 {}%", percent.min(100))
        }
        _ => format!(
            "下载中 {:.1} MB",
            done as f64 / (1024.0 * 1024.0)
        ),
    }
}

/// 失败那一句。
///
/// 试听片段单独说:那不是网络出了问题,重试一万次也还是只有 30 秒,
/// 而笼统的「下载失败」会让人一直点。
pub(in crate::music) fn describe_failure(
    err: &api::ApiError,
) -> String {
    if let api::ApiError::Server { code, .. } = err
        && code == api::TRIAL_ONLY
    {
        return "这首歌只有试听片段,下不了整首".to_owned();
    }
    format!("下载失败: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 知道总数就报百分比 —— 那是用户唯一看得懂的"还要多久"。
    #[test]
    fn a_known_total_reads_as_a_percentage() {
        assert_eq!(
            describe_progress(50, Some(200)),
            "下载中 25%"
        );
    }

    /// 服务端多发了几个字节(转码那一路的长度本来就是估的)也不能报出
    /// 101% —— 那会让人以为读数是坏的。
    #[test]
    fn progress_never_reads_past_a_hundred() {
        assert_eq!(
            describe_progress(300, Some(200)),
            "下载中 100%"
        );
    }

    /// 不知道总数时报已收的量,**不报百分比**:一条停在某个数不动的
    /// 百分比,比没有进度更像坏了。
    #[test]
    fn an_unknown_total_reads_as_bytes() {
        assert_eq!(
            describe_progress(3 * 1024 * 1024, None),
            "下载中 3.0 MB"
        );
        assert_eq!(
            describe_progress(0, Some(0)),
            "下载中 0.0 MB",
            "总数是 0 与不知道总数是同一件事,不能拿它当除数"
        );
    }

    /// 试听片段要单独说。重试一万次也还是只有 30 秒,而笼统的
    /// 「下载失败」会让人一直点。
    #[test]
    fn a_trial_only_refusal_says_what_it_is() {
        let err = api::ApiError::Server {
            code: api::TRIAL_ONLY.to_owned(),
            message: "只给得出试听片段".to_owned(),
        };

        assert_eq!(
            describe_failure(&err),
            "这首歌只有试听片段,下不了整首"
        );
    }

    /// 别的失败照常说出原因 —— 网络问题重试就好,不该被说成"要会员"。
    #[test]
    fn other_failures_keep_their_own_words() {
        let err = api::ApiError::Transport(
            "connection refused".to_owned(),
        );

        assert!(
            describe_failure(&err)
                .contains("connection refused"),
            "原因被吞掉了,用户只能看见一句下载失败"
        );
    }
}
