//! 界面这一侧的播放组(#142):服务端全局播放状态的接线。
//!
//! 服务端持有唯一一份状态,组里每台设备对等:点歌、切歌、暂停、拖动都是发给服务端的意图
//! (`api::group_*`,HTTP),服务端定序、改写、广播(信令的 `GroupState`)。这里做三件事:
//!
//! - 收事件(信令的后台线程上):组状态、各台的执行事实、连上与断开;
//! - 发意图(UI 线程上):成了就当场换上应答里的那一版,没成就说一句为什么;
//! - 把横幅、输出设备那一排、组那一行推到界面上。
//!
//! 本机怎么跟着状态出声在 `music::playback::group`;规则在 `app_core::GlobalGroup`。

mod rules;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use app_core::{
    DeviceReportDto, Effective, GlobalGroup, GroupPickDto,
    GroupSeedDto, GroupStateDto, Sound, Standing,
    TransportOpDto,
};
use slint::ComponentHandle;
use syncplay::{Client, Event};

pub(crate) use rules::{
    describe_copy_fault, describe_media_fault,
    describe_missing_entry, route_dto,
};

use crate::{MainWindow, Player, Shell};

/// 意图应答之后回头告诉发的那一侧:成了是 `Ok(组此刻那一版队列 (queue_id, revision))`
/// (组里没有歌或组散了是 `Ok(None)`),没成是 `Err(())`。电台靠它认出组队列里哪一版是
/// 它的,点歌回落本机靠它知道退组成了没有(#165)。
pub type Then =
    Box<dyn FnOnce(Result<Option<(i64, i64)>, ()>)>;

#[cfg(test)]
thread_local! {
    /// 测试里没有服务端:发出去还没应答的回调,由 [`Group::answer`] 代服务端回。
    static PENDING: std::cell::RefCell<Vec<Then>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 本机挂钟的毫秒。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 音乐页拿在手里的组把手。
#[derive(Clone)]
pub struct Group {
    inner: Arc<Inner>,
}

struct Inner {
    me: String,
    /// 信令客户端,由 `crate::sync::link` 建好交过来。事件回调要在 [`Client::start`]
    /// 之前就交出去,客户端要等它返回才拿得到,所以是 `OnceLock`。
    client: OnceLock<Arc<Client>>,
    view: Mutex<GlobalGroup>,
    /// 各台出声设备报上来的执行事实,按设备 id。只用于显示。
    reports: Mutex<HashMap<String, DeviceReportDto>>,
    /// 设备 id → 名字,来自名册。横幅上写的是人看得懂的那个。
    names: Mutex<HashMap<String, String>>,
    /// 名册上此刻在线的其他设备(不含本机)。
    online: Mutex<std::collections::HashSet<String>>,
    /// 点歌意图还在路上的那一首。同一首连点只发一次。
    in_flight: Mutex<Option<String>>,
    /// 已经报过放完的那一份 `(version, entry_id)`。每拍都看得见放空,只报一次。
    finished: Mutex<Option<(u64, i64)>>,
    /// 已经报过就绪的那一份 `(version, entry_id)`(#154)。同上,每拍都看得见,只报一次。
    readied: Mutex<Option<(u64, i64)>>,
    /// 封面已经取到哪一首了(只当遥控器时控制条的封面)。
    cover_id: Mutex<String>,
    /// 测试里记下发了哪些意图:测试里没有服务端。
    #[cfg(test)]
    intents: Mutex<Vec<String>>,
    weak: slint::Weak<MainWindow>,
}

/// 一个还没接上客户端的把手。先有它,再 [`Group::attach`]。
pub fn new(ui: &MainWindow, me: &str) -> Group {
    Group {
        inner: Arc::new(Inner {
            me: me.to_owned(),
            client: OnceLock::new(),
            view: Mutex::new(GlobalGroup::new(me)),
            reports: Mutex::new(HashMap::new()),
            names: Mutex::new(HashMap::new()),
            online: Mutex::new(Default::default()),
            in_flight: Mutex::new(None),
            finished: Mutex::new(None),
            readied: Mutex::new(None),
            cover_id: Mutex::new(String::new()),
            #[cfg(test)]
            intents: Mutex::new(Vec::new()),
            weak: ui.as_weak(),
        }),
    }
}

/// 一个谁也不连的把手,给测试用。
#[cfg(test)]
pub(crate) fn detached(ui: &MainWindow) -> Group {
    let group = new(ui, "me");
    group.attach(&Arc::new(Client::detached()));
    group
}

impl Group {
    /// 把信令客户端交给它。只认第一次。
    pub fn attach(&self, client: &Arc<Client>) {
        let _ = self.inner.client.set(client.clone());
    }

    pub fn me(&self) -> &str {
        &self.inner.me
    }

    pub fn standing(&self) -> Standing {
        lock(&self.inner.view).standing()
    }

    /// 本机在组里(出声或只当遥控器)。点歌入口据此发意图,而不是本机放。
    pub fn is_member(&self) -> bool {
        lock(&self.inner.view).is_member()
    }

    /// 组的出声设备里有没有一台此刻在线(本机算在线:信令连着)。一台都没有时组里点歌
    /// 哪儿都不响 —— 设备挂在没人用的旧组上(#165)。
    pub fn has_live_output(&self) -> bool {
        let view = lock(&self.inner.view);
        let Some(state) = view.state() else {
            return false;
        };
        let online = lock(&self.inner.online);
        state.outputs.iter().any(|id| {
            if *id == self.inner.me {
                view.is_online()
            } else {
                online.contains(id)
            }
        })
    }

    /// 信令连着没有。出声设备断开期间停着,连回来要按最新状态重新起这一首。
    pub fn is_online(&self) -> bool {
        lock(&self.inner.view).is_online()
    }

    /// 手上那一版全局状态。
    pub fn state(&self) -> Option<GroupStateDto> {
        lock(&self.inner.view).state().cloned()
    }

    /// 此刻本机该怎么出声。校时还没结论时没法换算,出声设备先停着。
    pub fn sound(&self) -> Sound {
        let view = lock(&self.inner.view);
        match self.server_now_us() {
            Some(now_us) => view.sound(now_us),
            None => match view.standing() {
                Standing::Solo => Sound::Solo,
                Standing::Remote => Sound::Silent,
                Standing::Output => Sound::Hold,
            },
        }
    }

    /// 全局状态里「此刻放的是什么」,不换算时刻。⏯、拖进度、队列页只要它就够了,
    /// 不必等校时出结论。
    pub fn now(&self) -> Option<app_core::GroupNowDto> {
        lock(&self.inner.view)
            .state()
            .and_then(|state| state.now.clone())
    }

    /// 此刻该放的那一条(按服务端时钟),控制条与队列页读它。
    pub fn effective(&self) -> Option<Effective> {
        let now_us = self.server_now_us()?;
        lock(&self.inner.view).effective(now_us)
    }

    // ── 校时 ──

    /// 服务端时钟此刻的读数(微秒)。还没校过时是 `None`。
    pub fn server_now_us(&self) -> Option<u64> {
        let client = self.inner.client.get()?;
        let clock = client.clock();
        let clock = clock.lock().ok()?;
        clock.to_server_us(audio::clock::monotonic_ns())
    }

    /// 服务端纪元 `epoch` 上的时刻换算成本机单调时钟(纳秒)。纪元对不上就是 `None`。
    pub fn to_local_ns(
        &self,
        epoch: u64,
        server_us: u64,
    ) -> Option<i64> {
        let client = self.inner.client.get()?;
        let clock = client.clock();
        let clock = clock.lock().ok()?;
        clock.to_local_ns(epoch, server_us)
    }

    // ── 事件(后台线程)──

    /// 处理一条信令事件。**在后台线程上**跑。
    pub fn handle(&self, event: &Event) {
        match event {
            Event::Connected => {
                lock(&self.inner.view).set_online(true);
            }
            // 掉线规则:出声设备与服务端断开就立刻停下,不按旧状态往下放。
            Event::Disconnected => {
                lock(&self.inner.view).set_online(false);
            }
            Event::GroupState(state) => {
                let state = state.as_deref().cloned();
                log::info!(
                    "组状态: {}",
                    state.as_ref().map_or_else(
                        || "没有组".to_owned(),
                        |state| format!(
                            "第 {} 版, 成员 {:?}, 出声 {:?}",
                            state.version,
                            state.members,
                            state.outputs
                        )
                    )
                );
                self.accept(state);
                return;
            }
            // 别的成员远程调本机的音量(#151):照本机拖滑块那样应用并存盘,
            // 下一条执行事实把新值带回给大家。
            Event::SetVolume { from, volume } => {
                log::info!(
                    "{from} 把本机音量调到 {volume}"
                );
                let volume = *volume;
                let _ = self
                    .inner
                    .weak
                    .upgrade_in_event_loop(move |ui| {
                        apply_remote_volume(&ui, volume)
                    });
                return;
            }
            Event::DeviceReport { from, report } => {
                let changed = lock(&self.inner.reports)
                    .insert(from.clone(), report.clone())
                    .as_ref()
                    != Some(report);
                if changed {
                    self.refresh();
                }
                return;
            }
            _ => return,
        }
        self.changed();
    }

    /// 收下一版(广播来的,或意图应答带回的)。旧的丢掉。
    fn accept(&self, state: Option<GroupStateDto>) {
        if lock(&self.inner.view).on_state(state) {
            self.changed();
        }
    }

    /// 名册变了:记下名字,横幅重算。
    pub fn set_names(
        &self,
        devices: &[app_core::DeviceDto],
    ) {
        *lock(&self.inner.online) = devices
            .iter()
            .map(|device| device.id.clone())
            .collect();
        {
            let mut names = lock(&self.inner.names);
            for device in devices {
                names.insert(
                    device.id.clone(),
                    device.name.clone(),
                );
            }
        }
        self.refresh();
    }

    /// 状态变了:叫 UI 线程重新对准本机播放,并重画那几行。
    fn changed(&self) {
        let _ =
            self.inner.weak.upgrade_in_event_loop(|ui| {
                ui.global::<Shell>().invoke_group_changed();
            });
        self.refresh();
    }

    /// 出声设备报本机的执行事实。不出声、没连上就不报。
    pub fn report(&self, report: DeviceReportDto) {
        let view = lock(&self.inner.view);
        if view.standing() != Standing::Output
            || !view.is_online()
        {
            return;
        }
        drop(view);
        if let Some(client) = self.inner.client.get() {
            client.report_device(report);
        }
    }

    // ── 意图(UI 线程)──

    /// 点歌单、卡墙、搜索里的一首。同一首的意图还在路上就不再发,说一句。
    pub fn play(
        &self,
        ui: &MainWindow,
        tracks: Vec<app_core::TrackDto>,
        index: usize,
    ) {
        self.play_then(ui, tracks, index, Box::new(|_| {}));
    }

    /// 同 [`Self::play`],应答之后调 `then`。
    pub fn play_then(
        &self,
        ui: &MainWindow,
        tracks: Vec<app_core::TrackDto>,
        index: usize,
        then: Then,
    ) {
        let Some(tapped) =
            tracks.get(index).map(|track| track.id.clone())
        else {
            return;
        };
        if self.redundant(ui, tapped.clone(), |now| {
            now.track.id == tapped
        }) {
            return;
        }
        self.send_then(
            ui,
            "点歌",
            format!("play {index}"),
            GroupIntent::Play(GroupPickDto::Tracks {
                tracks,
                index,
            }),
            then,
        );
    }

    /// 往组队列 `(queue_id, revision)` 那一版的队尾续几首(电台续歌,#165)。不是人按的,
    /// 没成不提示:电台自己冷却了再试。
    pub fn append(
        &self,
        ui: &MainWindow,
        (queue_id, revision): (i64, i64),
        tracks: Vec<app_core::TrackDto>,
        then: Then,
    ) {
        self.send_then(
            ui,
            "",
            format!("append {}", tracks.len()),
            GroupIntent::Append {
                queue_id,
                revision,
                tracks,
            },
            then,
        );
    }

    /// 队列页里的一条。
    pub fn pick(
        &self,
        ui: &MainWindow,
        queue_id: i64,
        revision: i64,
        entry_id: i64,
    ) {
        if self.redundant(
            ui,
            format!("entry {entry_id}"),
            |now| {
                now.queue_id == queue_id
                    && now.revision == revision
                    && now.entry_id == entry_id
            },
        ) {
            return;
        }
        self.send(
            ui,
            "切换",
            format!("pick {entry_id}"),
            GroupIntent::Play(GroupPickDto::Entry {
                queue_id,
                revision,
                entry_id,
            }),
        );
    }

    /// 点歌的连点去重,列表 / 卡墙 / 搜索与队列页共用(#142 F-1、N-1)。
    ///
    /// 多余的一下有两种:组此刻在放、放的就是这一首(`is_this`)—— 与本机那条路
    /// `rules::is_redundant_tap` 同一个判据;或者同一下的意图还在路上(`key`)。只看后者
    /// 不够:服务端几毫秒就回了,连点的第二下到时在途标记早清了。不多余就记下 `key` 为在途。
    fn redundant(
        &self,
        ui: &MainWindow,
        key: String,
        is_this: impl Fn(&app_core::GroupNowDto) -> bool,
    ) -> bool {
        let sounding = self.now().is_some_and(|now| {
            now.playing && is_this(&now)
        });
        let mut in_flight = lock(&self.inner.in_flight);
        if sounding
            || in_flight.as_deref() == Some(key.as_str())
        {
            crate::notice::show(
                ui,
                "这首已经在放或正在切过去".to_owned(),
            );
            return true;
        }
        *in_flight = Some(key);
        false
    }

    /// 本机(出声设备)真正放完了此刻那一首:报给服务端,最先报的那台推进(#142 AC-9)。
    /// 同一份 `(version, entry_id)` 只报一次。
    pub fn finished(&self, ui: &MainWindow) {
        let Some((version, entry_id)) = ({
            let view = lock(&self.inner.view);
            view.state().and_then(|state| {
                state
                    .now
                    .as_ref()
                    .filter(|now| now.playing)
                    .map(|now| {
                        (state.version, now.entry_id)
                    })
            })
        }) else {
            return;
        };
        if self.standing() != Standing::Output {
            return;
        }
        {
            let mut finished = lock(&self.inner.finished);
            if *finished == Some((version, entry_id)) {
                return;
            }
            *finished = Some((version, entry_id));
        }
        self.send(
            ui,
            "切换",
            format!("advance {entry_id}"),
            GroupIntent::Advance { entry_id, version },
        );
    }

    /// 本机(出声设备)把此刻那一首备好了,随时能出声:报给服务端。在线的出声设备都报了,
    /// 服务端把还没开走的起播提前(#154)。同一份 `(version, entry_id)` 只报一次。
    pub fn ready(&self, ui: &MainWindow, entry_id: i64) {
        let Some(version) = ({
            let view = lock(&self.inner.view);
            view.state()
                .filter(|state| {
                    state.now.as_ref().is_some_and(|now| {
                        now.entry_id == entry_id
                    })
                })
                .map(|state| state.version)
                .filter(|_| {
                    view.standing() == Standing::Output
                })
        }) else {
            return;
        };
        {
            let mut readied = lock(&self.inner.readied);
            if *readied == Some((version, entry_id)) {
                return;
            }
            *readied = Some((version, entry_id));
        }
        self.send(
            ui,
            "",
            format!("ready {entry_id}"),
            GroupIntent::Ready { entry_id, version },
        );
    }

    /// 控制条上的一下。
    pub fn transport(
        &self,
        ui: &MainWindow,
        op: TransportOpDto,
    ) {
        self.send(
            ui,
            "切换",
            format!("{op:?}"),
            GroupIntent::Transport(op),
        );
    }

    /// 改在这几台出声。本机的 id 就是 [`Self::me`]。
    pub fn set_outputs(
        &self,
        ui: &MainWindow,
        outputs: Vec<String>,
        seed: Option<GroupSeedDto>,
    ) {
        self.send(
            ui,
            "改输出设备",
            format!(
                "outputs {outputs:?}{}",
                if seed.is_some() { " +seed" } else { "" }
            ),
            GroupIntent::Outputs(outputs, seed),
        );
    }

    /// 组正在放时,在它现有的出声设备上加上 `device`(#149)。原有的一台都不动,不带种子:
    /// 组接着放它自己的那一首。没有组在放就是 `false`,什么都不发。
    pub fn add_output(
        &self,
        ui: &MainWindow,
        device: &str,
    ) -> bool {
        let Some(mut outputs) = self
            .state()
            .filter(|state| state.now.is_some())
            .map(|state| state.outputs)
        else {
            return false;
        };
        if !outputs.iter().any(|id| id == device) {
            outputs.push(device.to_owned());
        }
        self.set_outputs(ui, outputs, None);
        true
    }

    /// 远程调 `device` 的音量(#151)。音量条先跟手,真值等它下一条执行事实带回来。
    pub fn set_volume(
        &self,
        ui: &MainWindow,
        device: &str,
        volume: f32,
    ) {
        let volume = audio::clamped_volume(volume);
        set_row_volume(ui, device, volume);
        #[cfg(test)]
        lock(&self.inner.intents)
            .push(format!("volume {device} {volume:.2}"));
        if let Some(client) = self.inner.client.get() {
            client.set_volume(device, volume);
        }
    }

    /// 本机退出组,回到独奏。
    pub fn leave(&self, ui: &MainWindow) {
        self.leave_then(ui, Box::new(|_| {}));
    }

    /// 同 [`Self::leave`],应答之后调 `then`。
    pub fn leave_then(&self, ui: &MainWindow, then: Then) {
        self.send_then(
            ui,
            "退出组",
            "leave".to_owned(),
            GroupIntent::Leave,
            then,
        );
    }

    /// 测试里问:到此为止发了哪些意图。
    #[cfg(test)]
    pub(crate) fn intents(&self) -> Vec<String> {
        lock(&self.inner.intents).clone()
    }

    /// 测试里直接换上一版状态,当作服务端推来的。
    #[cfg(test)]
    pub(crate) fn assume(
        &self,
        state: Option<GroupStateDto>,
    ) {
        lock(&self.inner.view).set_online(true);
        lock(&self.inner.view).on_state(state);
    }

    /// 测试里代服务端应答:还没回的意图一律按 `queue` 回。
    #[cfg(test)]
    pub(crate) fn answer(
        &self,
        queue: Result<Option<(i64, i64)>, ()>,
    ) {
        for then in PENDING.with(|pending| pending.take()) {
            then(queue);
        }
    }

    fn send(
        &self,
        ui: &MainWindow,
        what: &'static str,
        label: String,
        intent: GroupIntent,
    ) {
        self.send_then(
            ui,
            what,
            label,
            intent,
            Box::new(|_| {}),
        );
    }

    fn send_then(
        &self,
        ui: &MainWindow,
        what: &'static str,
        label: String,
        intent: GroupIntent,
        then: Then,
    ) {
        log::info!("组意图: {label}");
        #[cfg(test)]
        {
            let _ = (ui, what, intent);
            lock(&self.inner.intents).push(label);
            PENDING.with(|pending| {
                pending.borrow_mut().push(then)
            });
        }
        #[cfg(not(test))]
        {
            let group = self.clone();
            let me = self.inner.me.clone();
            let weak = ui.as_weak();
            let _ = slint::spawn_local(async move {
                // 就绪与续歌不是人按的:没成不提示,也不清点歌的在途标记。
                let quiet = matches!(
                    intent,
                    GroupIntent::Ready { .. }
                        | GroupIntent::Append { .. }
                );
                let reply = match intent {
                    GroupIntent::Play(pick) => {
                        api::group_play(&me, pick).await
                    }
                    GroupIntent::Transport(op) => {
                        api::group_transport(&me, op).await
                    }
                    GroupIntent::Outputs(outputs, seed) => {
                        api::group_outputs(
                            &me, outputs, seed,
                        )
                        .await
                    }
                    GroupIntent::Leave => {
                        api::group_leave(&me).await
                    }
                    GroupIntent::Advance {
                        entry_id,
                        version,
                    } => {
                        api::group_advance(
                            &me, entry_id, version,
                        )
                        .await
                    }
                    GroupIntent::Ready {
                        entry_id,
                        version,
                    } => {
                        api::group_ready(
                            &me, entry_id, version,
                        )
                        .await
                    }
                    GroupIntent::Append {
                        queue_id,
                        revision,
                        tracks,
                    } => {
                        api::group_append(
                            &me, queue_id, revision, tracks,
                        )
                        .await
                    }
                };
                if !quiet {
                    lock(&group.inner.in_flight).take();
                }
                match reply {
                    Ok(state) => {
                        let queue = state
                            .as_ref()
                            .and_then(|state| {
                                state.now.as_ref()
                            })
                            .map(|now| {
                                (now.queue_id, now.revision)
                            });
                        group.accept(state);
                        then(Ok(queue));
                    }
                    Err(error) => {
                        then(Err(()));
                        log::warn!(
                            "组意图 {label} 没成: {error}"
                        );
                        if !quiet
                            && let Some(ui) = weak.upgrade()
                        {
                            crate::notice::show(
                                &ui,
                                rules::describe_intent_failure(
                                    what,
                                    &error.to_string(),
                                ),
                            );
                        }
                    }
                }
            });
        }
    }

    // ── 界面 ──

    fn name_of(&self, id: &str) -> String {
        if id == self.inner.me {
            return "本机".to_owned();
        }
        lock(&self.inner.names)
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_owned())
    }

    /// 组横幅那一句,以及那颗键是不是「加入」。空串是不显示。
    ///
    /// 独奏、组却在播:横幅说组在放什么,给一颗「加入」(#149)。暂停着不给:出声设备都关了
    /// 时组只是停着、成员也不清理,入口会在每台独奏设备上一直挂着。
    pub(crate) fn banner(&self) -> (String, bool) {
        let (standing, fellows, now) = {
            let view = lock(&self.inner.view);
            (
                view.standing(),
                view.fellow_outputs(),
                view.state()
                    .and_then(|state| state.now.clone()),
            )
        };
        if standing == Standing::Solo
            && let Some(now) = now.filter(|now| now.playing)
        {
            return (
                rules::describe_invite(&now.track.title),
                true,
            );
        }
        let fellows: Vec<String> = fellows
            .iter()
            .map(|id| self.name_of(id))
            .collect();
        (rules::describe_banner(standing, &fellows), false)
    }

    /// 横幅、输出设备那一排、组那一行推到界面上。
    pub fn refresh(&self) {
        let paint = self.painting();
        let _ = self
            .inner
            .weak
            .upgrade_in_event_loop(move |ui| paint(&ui));
    }

    /// 测试里当场画一遍:测试后端没有进程级的事件循环代理,
    /// `upgrade_in_event_loop` 送出去的闭包在那里一律被丢掉。
    #[cfg(test)]
    pub(crate) fn paint_now(&self, ui: &MainWindow) {
        self.painting()(ui);
    }

    /// 在这一侧把要画的算好,交回一个只在 UI 线程上跑的闭包。
    fn painting(
        &self,
    ) -> impl FnOnce(&MainWindow) + Send + 'static {
        let (standing, outputs, offline) = {
            let view = lock(&self.inner.view);
            (
                view.standing(),
                view.state()
                    .map(|state| state.outputs.clone())
                    .unwrap_or_default(),
                view.is_member() && !view.is_online(),
            )
        };
        let (banner, joinable) = self.banner();
        let member = standing != Standing::Solo;
        let output_text =
            rules::describe_output(&if member {
                outputs
                    .iter()
                    .map(|id| self.name_of(id))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            });
        // 出声的别台报上来的音量,那一排每台一条音量条(#151)。
        let volumes: HashMap<String, f32> = {
            let reports = lock(&self.inner.reports);
            outputs
                .iter()
                .filter(|id| **id != self.inner.me)
                .filter_map(|id| {
                    Some((
                        id.clone(),
                        reports.get(id)?.volume?,
                    ))
                })
                .collect()
        };
        let line = {
            let reports = lock(&self.inner.reports);
            if member {
                rules::describe_group(
                    &outputs,
                    &reports,
                    |id| self.name_of(id),
                )
            } else {
                String::new()
            }
        };
        // 输出设备那一排:在组里时标出正在出声的那几台(本机是空串);独奏时本机亮着。
        let marked: Vec<String> = if member {
            outputs
                .iter()
                .map(|id| {
                    if *id == self.inner.me {
                        String::new()
                    } else {
                        id.clone()
                    }
                })
                .collect()
        } else {
            vec![String::new()]
        };
        let first = marked
            .iter()
            .find(|id| !id.is_empty())
            .cloned()
            .filter(|_| !marked.contains(&String::new()))
            .unwrap_or_default();
        move |ui: &MainWindow| {
            let shell = ui.global::<Shell>();
            shell.set_group_banner(banner.into());
            shell.set_group_joinable(joinable);
            shell.set_output_text(output_text.into());
            shell.set_output_id(first.into());
            shell.set_group_text(line.into());
            shell.set_output_stale(offline);
            mark_members(ui, &marked, &volumes);
        }
    }

    /// 只当遥控器时控制条画全局状态:歌名、进度、在不在放。
    pub fn push_playback(&self, ui: &MainWindow) {
        let Some(now) = self.effective() else {
            ui.global::<Player>().set_has_track(false);
            ui.global::<Player>().set_is_playing(false);
            return;
        };
        let position_ms = self.server_now_us().map_or(
            now.position_us,
            |at| {
                if now.playing && at > now.anchor_us {
                    now.position_us + (at - now.anchor_us)
                } else {
                    now.position_us
                }
            },
        ) / 1_000;
        let track = now.track;
        self.sync_cover(ui, &track);
        ui.global::<crate::Viz>()
            .set_now_title(track.title.clone().into());
        ui.global::<crate::Viz>().set_now_artists(
            crate::music::join_artists(&track.artists)
                .into(),
        );
        let seconds = position_ms as f64 / 1_000.0;
        ui.global::<Player>().set_has_track(true);
        ui.global::<Player>().set_is_playing(now.playing);
        ui.global::<Player>().set_buffering(false);
        ui.global::<Player>()
            .set_now_id(track.id.clone().into());
        ui.global::<Player>().set_playback_text(
            if now.playing {
                format!("组: 正在播放 {}", track.title)
            } else {
                "组: 已暂停".to_owned()
            }
            .into(),
        );
        ui.global::<Player>().set_progress_ratio(
            crate::progress::ratio(
                seconds,
                track.duration_ms,
            ),
        );
        ui.global::<Player>().set_progress_text(
            crate::progress::progress_text(
                seconds,
                track.duration_ms,
            )
            .into(),
        );
    }

    /// 控制条的封面跟着全局状态那一首走。换歌那一拍取一次。
    fn sync_cover(
        &self,
        ui: &MainWindow,
        track: &app_core::TrackDto,
    ) {
        {
            let mut held = lock(&self.inner.cover_id);
            if *held == track.id {
                return;
            }
            held.clone_from(&track.id);
        }
        ui.global::<crate::Viz>()
            .set_cover_art(slint::Image::default());
        let Some(url) = track.cover.clone() else {
            return;
        };
        let id = track.id.clone();
        let group = self.clone();
        let weak = ui.as_weak();
        let _ = slint::spawn_local(async move {
            let Ok(bytes) = api::fetch_bytes(&url).await
            else {
                return;
            };
            let wanted = {
                let (group, id) =
                    (group.clone(), id.clone());
                move || *lock(&group.inner.cover_id) == id
            };
            let Some(decoded) =
                crate::imagery::cover::decode_off_thread(
                    bytes, wanted,
                )
                .await
            else {
                return;
            };
            if *lock(&group.inner.cover_id) != id {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.global::<crate::Viz>().set_cover_art(
                    slint::Image::from_rgba8(decoded.full),
                );
            }
        });
    }
}

/// 发出去的是哪一种意图。测试里不发网络,内容只记进 `intents`。
#[cfg_attr(test, allow(dead_code))]
enum GroupIntent {
    Play(GroupPickDto),
    Transport(TransportOpDto),
    Outputs(Vec<String>, Option<GroupSeedDto>),
    Leave,
    Advance {
        entry_id: i64,
        version: u64,
    },
    Ready {
        entry_id: i64,
        version: u64,
    },
    Append {
        queue_id: i64,
        revision: i64,
        tracks: Vec<app_core::TrackDto>,
    },
}

/// 名册那一排芯片上标出谁在出声(空串是本机)。「加入 / 移出」那颗小键照它显示。
pub(crate) fn mark_members(
    ui: &MainWindow,
    member_ids: &[String],
    volumes: &HashMap<String, f32>,
) {
    use slint::Model as _;

    let rows = ui.global::<Shell>().get_devices();
    for index in 0..rows.row_count() {
        let Some(row) = rows.row_data(index) else {
            continue;
        };
        let member = member_ids
            .iter()
            .any(|id| *id == row.id.as_str());
        let volume = volumes.get(row.id.as_str()).copied();
        let updated = crate::DeviceRow {
            member,
            has_volume: volume.is_some(),
            volume: volume.unwrap_or(row.volume),
            ..row.clone()
        };
        if updated != row {
            rows.set_row_data(index, updated);
        }
    }
    ui.global::<Shell>().set_local_member(
        member_ids.iter().any(String::is_empty),
    );
}

/// 那一排里 `device` 那一行的音量条改成 `volume`。
fn set_row_volume(
    ui: &MainWindow,
    device: &str,
    volume: f32,
) {
    use slint::Model as _;

    let rows = ui.global::<Shell>().get_devices();
    for index in 0..rows.row_count() {
        if let Some(mut row) = rows.row_data(index)
            && row.id == device
        {
            row.volume = volume;
            rows.set_row_data(index, row);
        }
    }
}

/// 别的成员调了本机的音量(#151):走的是本机拖滑块那条路,夹值、应用、节流存盘都在那里。
pub(crate) fn apply_remote_volume(
    ui: &MainWindow,
    volume: f32,
) {
    ui.global::<Player>().invoke_volume_changed(volume);
}

/// 把组接到界面上:横幅上的「退出」与「加入」,以及远程调音量。
pub fn bind(ui: &MainWindow, group: &Group) {
    let tuning = group.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_set_device_volume(
        move |device, volume| {
            let Some(ui) = weak.upgrade() else { return };
            tuning.set_volume(&ui, &device, volume);
        },
    );

    let joining = group.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_join_group(move || {
        let Some(ui) = weak.upgrade() else { return };
        joining.add_output(&ui, &joining.inner.me);
    });

    let leaving = group.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_leave_group(move || {
        let Some(ui) = weak.upgrade() else { return };
        leaving.leave(&ui);
    });
    group.refresh();
}

fn lock<T>(
    value: &Mutex<T>,
) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("组状态锁中毒")
}
