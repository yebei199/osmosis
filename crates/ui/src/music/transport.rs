//! 传输核心:起播一首、播放状态的每帧推送,以及放完自动接上下一首。

use super::*;
use crate::Player;
use slint::ComponentHandle as _;

/// ⏯ 这一下:退出收听 / 暂停 / 继续 / 重放。
///
/// 从回调里抽出来,是因为系统媒体控件按的也是这一下 —— 那边不该有第二套说法
/// (见 [`dispatch_media`])。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn toggle_play(ui: &MainWindow, deck: &Deck) {
    if deck.sync.is_listening() {
        deck.sync.leave();
        if let Ok(player) = deck.player.as_ref() {
            player.stop();
        }
        ui.global::<Player>().set_is_playing(false);
        return;
    }

    let Ok(player) = deck.player.as_ref() else {
        return;
    };
    if ui.global::<Player>().get_is_playing() {
        player.pause();
        ui.global::<Player>().set_is_playing(false);
    } else if !player.empty() {
        // 暂停中,接着放。
        player.resume();
        ui.global::<Player>().set_is_playing(true);
    } else {
        // 放空了(队列结束后又按了播放):重放当前这首。
        play_current(ui, deck);
    }
}

/// 放队列的当前曲目:取直链 → 开流 → 解码 → 出声,经 `app_core::play` 记账。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn play_current(ui: &MainWindow, deck: &Deck) {
    let Some(track) =
        deck.queue.borrow().current().cloned()
    else {
        return;
    };

    // 备好的那一份先取走 —— 取不到就是走原路。这一步要在停旧歌之前:
    // 备着的话下面那段等待根本不存在,声音接上就换。
    let ready =
        take_prefetched(&deck.prefetched, &track.id);
    let instant = ready.is_some();

    // **旧歌立刻停。** 界面下面几行就要换成新歌了,让耳朵继续听上一首是自相矛盾
    // ——「封面换了但还在放上一首」正是这么来的。备好了的话这一停是零长度的。
    if let Ok(player) = deck.player.as_ref() {
        player.stop();
    }
    ui.global::<Player>().set_is_playing(false);
    ui.global::<Player>().set_now_loading(!instant);

    // spawn_local 的 future 要到下一轮事件循环才跑,而 Loading 要立刻显示。
    //
    // 状态先写进 `playback`,再让状态行与列表都从它读:列表那一行的加载态
    // 是用户手指底下唯一看得见的反馈,晚一帧就等于没有。
    ui.global::<Player>().set_playback_text(
        describe_playback(&PlaybackState::Loading(
            track.clone(),
        ))
        .into(),
    );
    push_rows(ui, deck, Some(&track.id));

    // 播放页的歌名与封面。旧封面立刻清掉 —— 新歌配旧图比空着更误导。
    ui.set_now_title(track.title.clone().into());
    ui.set_now_artists(join_artists(&track.artists).into());
    ui.set_cover_art(slint::Image::default());

    // 歌词也随歌换:先清空(旧歌词配新歌比空着更误导),取到再整批换上。
    // 取不到不影响播放 —— 没歌词是正常状态,不是故障(见 crates/contract)。
    deck.lyrics.replace(Vec::new());
    ui.set_lyric_line(slint::SharedString::new());
    ui.set_lyric_translation(slint::SharedString::new());
    {
        let lyrics = deck.lyrics.clone();
        let id = track.id.clone();
        slint::spawn_local(async move {
            if let Ok(dto) = api::lyric(&id).await {
                lyrics.replace(dto.lines);
            }
        })
        .expect("event loop must be running");
    }

    // 点云也跟着清:与上面那两样同一条原则。少了这一步,取封面的那几百毫秒里
    // 点云仍是上一首;而封面取不到时(CDN 会过期、有的歌根本没有封面)它会
    // **一直**是上一首(见 `docs/adr/0014` 与 `CONTEXT.md`「封面点云」)。
    deck.cover.clear();
    // 极光的封面色同理:旧色配新歌比主题绿更误导(aurora.rs)。
    crate::aurora::reset(ui);
    // 媒体控件那份同理:锁屏上挂着上一首的封面,比空着更误导。
    deck.media.clear_art();

    if let Some(url) = track.cover.clone() {
        let weak = ui.as_weak();
        let cover = deck.cover.clone();
        let media = deck.media.clone();
        let playback = deck.playback.clone();
        let id = track.id.clone();
        slint::spawn_local(async move {
            // 拿不到或解不出就保持空图:封面 CDN 会过期,失败是常态(见 cover.rs)。
            // 同一次解码喂两处:界面的封面卡,以及点云的采样纹理。
            if let Ok(bytes) = api::fetch_bytes(&url).await
                && let Some((img, pixels)) =
                    crate::cover::decode(&bytes)
                && let Some(ui) = weak.upgrade()
            {
                // 连按下一首时,先发的请求可能后回来。到这时它已经不是当前这首,
                // 换上去就是「A 的封面配 B 的歌」—— 与 `app_core::play` 的代际
                // 校验同一个道理,只是这里对得上 id 就够了。
                if current_id(playback.borrow().state())
                    != Some(id.as_str())
                {
                    return;
                }
                ui.set_cover_art(img);
                // 一张图四个去处:封面卡、点云、媒体控件,以及极光的三团光斑。
                // `Arc` 免掉后面几个各拷一份兆级字节。
                let pixels = Arc::new(pixels);
                crate::aurora::feed(&ui, &pixels);
                cover.replace(pixels.clone());
                media.set_art(pixels);
                crate::media::push(&ui, &playback, &media);
            }
        })
        .expect("event loop must be running");
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let commit = deck.clone();
        let player = deck.player.clone();
        app_core::play(
            &deck.playback,
            track,
            move |track| async move {
                // 备好了就直接交出去 —— 与现取的那份走同一个类型、同一段提交路径,
                // 差别只有"等不等"。
                match ready {
                    Some(ready) => Ok(ready),
                    None => prepare(player, track).await,
                }
            },
            move |(decoded, health)| {
                emit(
                    &commit.player,
                    &commit.sync,
                    &commit.stream,
                    &commit.seeking,
                    decoded,
                    health,
                );
            },
        )
        .await;

        if let Some(ui) = weak.upgrade() {
            let (playing, text) = {
                let state = deck.playback.borrow();
                (
                    matches!(
                        state.state(),
                        PlaybackState::Playing(_)
                    ),
                    describe_playback(state.state()),
                )
            };
            ui.global::<Player>().set_is_playing(playing);
            ui.global::<Player>()
                .set_playback_text(text.into());
            ui.global::<Player>().set_now_loading(false);
            // 放起来了就把断流横幅收掉:声音回来了,那句话已经过期。
            if playing {
                ui.set_banner_text(
                    slint::SharedString::new(),
                );
            }
            // 这一首要么放起来了、要么失败了,行上的加载态该收了。
            // 被顶掉的那次连这里都到不了 —— `app_core::play` 提前返回。
            push_rows(&ui, &deck, None);
            // 换歌立刻报出去。等下一次轮询是 1 秒之后,锁屏上会慢半拍。
            crate::media::push(
                &ui,
                &deck.playback,
                &deck.media,
            );
        }
    })
    .expect("event loop must be running");
}

/// 自动续播:每秒看一眼,放空了就推进队列。
///
/// rodio 没有"放完了"的回调,轮询是唯一的办法;判据抽在 [`should_advance`],
/// 收听同播时它恒为假 —— 那时切歌会把对面推来的声音捣掉。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn start_auto_advance(
    ui: &MainWindow,
    deck: &Deck,
) {
    let deck = deck.clone();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    // 上次报过的那一首。起播上报搭这趟轮询的车,判据见 `play_to_report`。
    let reported: RefCell<Option<(String, String)>> =
        RefCell::new(None);

    timer.start(
        slint::TimerMode::Repeated,
        ADVANCE_POLL,
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let (drained, position) =
                match deck.player.as_ref() {
                    Ok(player) => {
                        (player.empty(), player.position())
                    }
                    Err(_) => {
                        (true, core::time::Duration::ZERO)
                    }
                };
            // 卡住时这一行是唯一的判据:位置冻住而没放空 = 声卡回调被网络读堵住
            // (见 `audio` 的 PREFETCH_BYTES);位置反复归零 = 这一首被重放了。
            // 两种症状听起来一模一样,数出来才分得开。
            log::debug!(
                "自动续播轮询: 位置 {position:?}, 放空 {drained}"
            );

            let gave_up = deck
                .stream
                .borrow()
                .as_ref()
                .is_some_and(audio::StreamHealth::gave_up);
            let state = deck.playback.borrow().state().clone();
            let listening = deck.sync.is_listening();

            // 进度搭这趟车,不另起一个定时器:位置已经在上面取过了,
            // 而两个定时器意味着两套"现在放到哪"的说法。
            push_progress(&ui, &state, position);
            push_seek_state(&ui, &deck);
            // 媒体控件搭同一趟车。它自己去重,平帧推出去的是零个字节。
            crate::media::push(&ui, &deck.playback, &deck.media);

            // 起播上报也搭这趟车:个人主页的统计从这条账本查询时聚合
            // (server 的 `play_events`)。报失败只写日志 —— 统计不该打断听歌。
            if let Some((platform, id)) =
                play_to_report(&state, &mut reported.borrow_mut())
            {
                slint::spawn_local(async move {
                    if let Err(error) =
                        api::record_play(&platform, &id).await
                    {
                        log::debug!("起播上报没成: {error}");
                    }
                })
                .expect("event loop must be running");
            }

            // 断流先判:两个出口在同一刻都可能成立,而断了就不该切歌 ——
            // 网没了下一首同样放不出来,一分钟能把整个队列烧光。
            if should_report_loss(
                &state, drained, listening, gave_up,
            ) {
                report_stream_loss(&ui, &deck);
            } else if should_advance(&state, drained, listening)
            {
                advance_auto(&ui, &deck);
            }

            // 备下一首。判据抽在 `should_prefetch`,这里只负责把当下的事实凑齐。
            let already_have = deck.prefetching.get()
                || deck.prefetched.borrow().is_some();
            let has_next =
                deck.queue.borrow().peek_next().is_some();
            if should_prefetch(
                &state,
                position,
                listening,
                already_have,
                has_next,
            ) {
                start_prefetch(&deck);
            }
        },
    );

    // ponytail: 定时器与进程同寿,leak 掉省一条把 Timer 递回平台入口的通道;
    // 真要按页开关时再把它挂到 Deck 上管理。
    Box::leak(Box::new(timer));
}

/// 把当前进度推给界面。
///
/// 手上没歌时清成「没有」而不是留着上一首的数字 —— 停下之后那条进度条
/// 还停在 3:41,读起来像是还在放。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn push_progress(
    ui: &MainWindow,
    state: &PlaybackState,
    position: core::time::Duration,
) {
    let track = match state {
        PlaybackState::Playing(track)
        | PlaybackState::Loading(track) => track,
        _ => {
            ui.global::<Player>().set_has_track(false);
            return;
        }
    };

    let secs = position.as_secs_f64();
    ui.global::<Player>().set_has_track(true);
    ui.global::<Player>().set_progress_ratio(
        crate::progress::ratio(secs, track.duration_ms),
    );
    ui.global::<Player>().set_progress_text(
        crate::progress::progress_text(
            secs,
            track.duration_ms,
        )
        .into(),
    );
}
