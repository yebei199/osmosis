//! Linux 这一端的 MPRIS 实现:占名、开服务,以及播放器接口本身。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

mod map;
mod root_iface;

use map::{
    loop_mode_of, loop_status_name, metadata_of,
    status_name, track_path,
};
use root_iface::Root;

/// 总线上的名字。外壳按这个前缀扫,后缀是谁不重要,但得是我们自己。
pub(super) const BUS_NAME: &str =
    "org.mpris.MediaPlayer2.osmosis";

/// 规范钉死的对象路径,两个接口都挂在它上面。
pub(super) const OBJECT_PATH: &str =
    "/org/mpris/MediaPlayer2";

pub(super) const PLAYER_IFACE: &str =
    "org.mpris.MediaPlayer2.Player";

pub(super) const PROPERTIES_IFACE: &str =
    "org.freedesktop.DBus.Properties";

/// 曲目路径的前缀。规范只要求是合法 object path 且能区分曲目,前缀由我们定。
pub(super) const TRACK_PREFIX: &str =
    "/io/github/osmosis/track/";

/// 规范给「什么都没放」留的那条专用路径。
pub(super) const NO_TRACK: &str =
    "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// MPRIS 的时间单位是微秒,seam 那边统一是毫秒。
pub(super) const MICROS_PER_MILLI: i64 = 1_000;

/// 起服务。连不上就退回什么都不做 —— 没有 MPRIS 不影响出声。
///
/// 会连不上的情形都很正常:没有 session bus(纯 tty、容器里),或者名字
/// 已经被另一个实例占了(启动锁挡不住 `--user` 之外的场景)。
pub fn start(
    hooks: ui::MediaHooks,
) -> Box<dyn ui::MediaControls> {
    match serve(None, hooks) {
        Ok(mpris) => Box::new(mpris),
        Err(err) => {
            log::warn!(
                "接不上 MPRIS,系统媒体控件里不会有这个播放器: {err}"
            );
            Box::new(ui::NoControls)
        }
    }
}

/// 两个接口共用的那一份。
pub(super) struct Shared {
    hooks: ui::MediaHooks,
    /// 上一次推出去的东西。属性被拉时从这里答。
    now: Mutex<ui::NowPlaying>,
}

impl Shared {
    fn send(&self, command: ui::MediaCommand) {
        (self.hooks.command)(command);
    }

    /// 锁中毒了也照常答:里面是一份纯数据的快照,毒不着它。
    fn now(&self) -> ui::NowPlaying {
        self.now
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// 已经接上总线的 MPRIS。`conn` 必须活着 —— 丢了它名字就还回去了。
pub(super) struct Mpris {
    conn: zbus::blocking::Connection,
    shared: Arc<Shared>,
}

impl ui::MediaControls for Mpris {
    fn publish(&self, now: &ui::NowPlaying) {
        *self
            .shared
            .now
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            now.clone();

        // 属性变更要自己吆喝一声,否则对面只有在下次主动来问时才知道 ——
        // 而 bar 上的卡片画完就不再问了,于是永远停在第一首歌。
        let mut changed: HashMap<&str, Value<'_>> =
            HashMap::new();
        changed.insert(
            "PlaybackStatus",
            Value::from(status_name(now.status)),
        );
        changed.insert(
            "Metadata",
            Value::from(metadata_of(now)),
        );
        changed.insert("Shuffle", Value::from(now.shuffle));
        changed.insert(
            "LoopStatus",
            Value::from(loop_status_name(now.loop_mode)),
        );

        if let Err(err) = self.conn.emit_signal(
            None::<()>,
            OBJECT_PATH,
            PROPERTIES_IFACE,
            "PropertiesChanged",
            &(PLAYER_IFACE, changed, &[] as &[&str]),
        ) {
            log::debug!("MPRIS 属性变更发不出去: {err}");
        }
    }
}

/// 接上总线并注册两个接口。
///
/// `address` 为 `None` 走会话总线;测试传一条临时总线的地址,免得往用户
/// 自己的总线上摆一个假播放器。
pub(super) fn serve(
    address: Option<&str>,
    hooks: ui::MediaHooks,
) -> zbus::Result<Mpris> {
    let shared = Arc::new(Shared {
        hooks,
        now: Mutex::new(ui::NowPlaying::default()),
    });

    let builder = match address {
        Some(address) => {
            zbus::blocking::connection::Builder::address(
                address,
            )?
        }
        None => {
            zbus::blocking::connection::Builder::session()?
        }
    };

    // 两个接口挂同一条路径,规范如此。`name` 放在最后:名字要不到时
    // 连接就不该建起来。
    let conn = builder
        .serve_at(OBJECT_PATH, Root)?
        .serve_at(OBJECT_PATH, Player(shared.clone()))?
        .name(BUS_NAME)?
        .build()?;

    Ok(Mpris { conn, shared })
}

/// 播放器接口:正在放什么,以及外面能按什么。
pub(super) struct Player(Arc<Shared>);

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play_pause(&self) {
        self.0.send(ui::MediaCommand::Toggle);
    }

    /// `Play` 与 `Pause` 原样转发,不在这里判「现在是不是在放」——
    /// 那道判断归 `ui::media`,后端记第二份状态迟早会与界面对不上。
    fn play(&self) {
        self.0.send(ui::MediaCommand::Play);
    }

    fn pause(&self) {
        self.0.send(ui::MediaCommand::Pause);
    }

    /// 没有「停止」这个动作 —— 队列还在,只是不出声。当暂停处理。
    fn stop(&self) {
        self.0.send(ui::MediaCommand::Pause);
    }

    fn next(&self) {
        self.0.send(ui::MediaCommand::Next);
    }

    fn previous(&self) {
        self.0.send(ui::MediaCommand::Previous);
    }

    /// 相对跳转,入参微秒可负。
    fn seek(&self, offset: i64) {
        self.0.send(ui::MediaCommand::SeekBy(
            offset / MICROS_PER_MILLI,
        ));
    }

    /// 绝对跳转。曲目对不上就忽略 —— 规范如此,而且那多半是 bar 上那张
    /// 卡片还停在上一首,照做就是跳错歌。
    fn set_position(&self, track: ObjectPath<'_>, at: i64) {
        if track.as_str()
            != track_path(&self.0.now().track_id)
        {
            return;
        }
        self.0.send(ui::MediaCommand::SeekTo(
            at / MICROS_PER_MILLI,
        ));
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        status_name(self.0.now().status).to_owned()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata_of(&self.0.now())
    }

    /// 随机开着没有。可写 —— bar 上那颗随机键按下去走的是这条 setter。
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.0.now().shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&self, want: bool) {
        self.0.send(ui::MediaCommand::SetShuffle(want));
    }

    /// 循环三态。可写 —— bar 上那颗循环键走的是这条 setter。
    #[zbus(property)]
    fn loop_status(&self) -> String {
        loop_status_name(self.0.now().loop_mode).to_owned()
    }

    /// 不认识的值不折腾:规范只有三个取值,别的都是对面写错了。
    #[zbus(property)]
    fn set_loop_status(&self, want: String) {
        let Some(mode) = loop_mode_of(&want) else {
            log::debug!(
                "MPRIS 送来不认识的 LoopStatus: {want}"
            );
            return;
        };
        self.0.send(ui::MediaCommand::SetLoop(mode));
    }

    /// 位置每时每刻都在变,规范明说它**不**走 `PropertiesChanged` ——
    /// 对面要么来问,要么自己按倍率外推。
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        (self.0.hooks.position)().as_micros() as i64
    }

    // ponytail: 音量恒 1.0 且只读。bar 上那条音量条通常本来就是系统音量,
    // 真要接就给 `MediaHooks` 再加一对读写的线。只读比「写了但没生效」诚实。
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    // rodio 不变速,三个都恒为 1。
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    /// 跳转能不能成要到解码线程才知道(见 `docs/adr/0019`),这里只能说
    /// 「按钮该画出来」。跳不动时界面自己会说话。
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

#[cfg(test)]
pub(super) mod tests;
