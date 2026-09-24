//! 输出设备:声音从哪台设备出来,以及遥控器手上那份被控端状态的镜像。
//!
//! 「输出设备 = 谁」**就是**遥控器模式的全部状态,不多一个模式开关(产品规则)。
//! 选本机就是现在的行为,选别的设备则把播放动作换成一条命令、把播放状态换成
//! 对方的上报。界面层只认这一个抽象。
//!
//! 本 crate 是纯规则层、不碰时钟(`docs/adr/0002`),所以每个要「现在几点」的
//! 方法都由调用方把毫秒传进来 —— 与 [`crate::Queue`] 收 `seed`、
//! [`crate::play`] 收两个闭包是同一条纪律。插值与过期因此不必靠 sleep 去测。

use contract::{
    DeviceDto, RemotePlayState, RemoteStateDto, TrackDto,
};

/// 声音从哪台设备出来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Output {
    /// 本机。现有的那条路径一个字节都不变。
    #[default]
    Local,
    /// 交给这台设备放。本机不出声,只发命令、只显示对方的上报。
    Remote(DeviceDto),
}

impl Output {
    /// 命令要发去哪台设备。本机输出时 `None` —— 那条路根本不发信令。
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Remote(device) => Some(&device.id),
        }
    }

    /// 这台设备叫什么,给界面显示。
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Remote(device) => Some(&device.name),
        }
    }
}

/// 连着多久收不到上报就算「状态已过期」。
///
/// 三秒是一秒一次上报的三倍:漏一次是抖动,漏三次是真的断了。过期期间禁用
/// 控制,但**不自动切回本机**(产品规则)—— 切回去的话用户一低头就发现歌
/// 从手机里放出来了,而他要的是让 pc1 放。
const STALE_AFTER_MS: u64 = 3_000;

/// 连着多久收不到上报就判定「这台设备失联了」,自动回本机。
///
/// 与 [`STALE_AFTER_MS`] 是两档,不是一档:过期只是「这几秒的状态不可信」,
/// 而这一档是「它不会再回来了」。中间隔着十几秒,是留给一次正常的网络抖动 ——
/// 抖一下就把人踢回本机,比停在过期上更烦人。
///
/// 非有不可:撤权是服务端尽力而为的一发(`server::syncplay::control` 的 `send` 用
/// `try_send` 且不重发),丢了之后遥控器的 socket 还好好的、不会重连,
/// 于是它永久停在「遥控: 状态已过期」,谁也不来告诉它一声(#102 F-003)。
const LOST_AFTER_MS: u64 = 15_000;

/// 遥控器手上那份被控端状态。
///
/// 只是一面镜子:队列、音量、进度全是被控端报过来的,遥控器不持有自己的那一份
/// (`docs/adr/0030`)。它唯一自己算的东西是两次上报之间的插值。
#[derive(Debug, Default)]
pub struct RemoteView {
    report: Option<RemoteStateDto>,
    /// 上一份上报**到达**的本地时刻,毫秒。
    ///
    /// 用本地钟而不是报文里的 `sent_at`:两台设备的钟本来就不一样,拿它算
    /// 经过了多久只会算出负数或者几个小时。
    received_at_ms: u64,
}

impl RemoteView {
    /// 收下一份上报。返回它有没有被收下 —— 比手上这份旧的会被丢掉。
    ///
    /// 迟到的旧上报必须丢:重连之后旧连接上的残余可能后到,收下它进度条就会
    /// 倒退一次,而那看起来正好像是「拖进度失败了」。
    ///
    /// 排序键是 `(epoch, state_seq)` 这一对,不是任何一个单独拿出来:
    ///
    /// - 只比 `state_seq`:被控端一重启序号从头数,新进程报来的一切都排在
    ///   旧进程后面,于是全被丢掉,界面停在旧状态上看起来像「对面没反应」。
    /// - 只比 `epoch`:同一次会话里的所有上报不可比,乱序的残余照收不误。
    ///
    /// 这一对与服务端 `play_queue_reports` 收报告时用的是同一个序 ——
    /// 同一份执行状态走 WebSocket 与 HTTP 两条路上报,两条得排得到一起。
    pub fn accept(
        &mut self,
        report: RemoteStateDto,
        now_ms: u64,
    ) -> bool {
        if self.report.as_ref().is_some_and(|held| {
            (report.epoch, report.state_seq)
                < (held.epoch, held.state_seq)
        }) {
            return false;
        }
        self.report = Some(report);
        self.received_at_ms = now_ms;
        true
    }

    /// 切回本机:忘掉上一台设备的一切。
    ///
    /// 留着的话,下一次接管另一台设备会先闪一眼上一台的歌名。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 收到过至少一条上报。
    ///
    /// 与「过期」是两回事,分开是因为它们该导向相反的行为:刚接管、快照还在
    /// 路上时,控制**要能发出去** —— 从选中设备到第一条上报回来是三个来回,
    /// 这期间挡住的话,选完设备马上点一首歌就会被静默丢掉,而那正是
    /// 「按了没反应」。而真的过期时必须挡住:那份状态已经不知道被控端在
    /// 干什么了。
    pub fn is_known(&self) -> bool {
        self.report.is_some()
    }

    /// 收到过上报,但连着 [`STALE_AFTER_MS`] 没有新的了。
    ///
    /// 一条都还没收到时**不算过期**,算「还不知道」—— 见 [`Self::is_known`]。
    pub fn is_stale(&self, now_ms: u64) -> bool {
        self.is_known()
            && now_ms.saturating_sub(self.received_at_ms)
                > STALE_AFTER_MS
    }

    /// 收到过上报,但连着 [`LOST_AFTER_MS`] 没有新的了 —— 这台设备失联了。
    ///
    /// 与 [`Self::is_stale`] 同一个形状、不同的档位与去向:过期只是禁用控制,
    /// 失联要把输出切回本机。一条都没收到过时不算失联,那是「还没开始」。
    pub fn is_lost(&self, now_ms: u64) -> bool {
        self.is_known()
            && now_ms.saturating_sub(self.received_at_ms)
                > LOST_AFTER_MS
    }

    /// 现在该显示到第几毫秒。
    ///
    /// 正在放且没过期时按本地时钟插值 —— 不插的话进度条每秒跳一格,而被控端
    /// 明明在连续地放。缓冲与暂停时原样返回上报的位置:那一刻进度并没有在走,
    /// 照插会让进度条自己往前爬、再在下一次上报时跳回去。
    pub fn position_ms(&self, now_ms: u64) -> u64 {
        let Some(report) = &self.report else {
            return 0;
        };
        if report.state != RemotePlayState::Playing {
            return report.position_ms;
        }

        // 过期之后停止插值:这条进度条已经不知道真相了,别让它接着爬。
        let elapsed = now_ms
            .saturating_sub(self.received_at_ms)
            .min(STALE_AFTER_MS);
        let position = report.position_ms + elapsed;
        match &report.track {
            // 越过曲长就会显示 3:25 / 3:20,那种数字一出现,
            // 用户就再也不信这条进度条。
            Some(track) => position
                .min(track.duration_ms.max(0) as u64),
            None => position,
        }
    }

    /// 被控端此刻在干什么。一条上报都没有时按空闲算。
    pub fn state(&self) -> RemotePlayState {
        self.report
            .as_ref()
            .map_or(RemotePlayState::Idle, |report| {
                report.state
            })
    }

    /// 正在放的那首。
    pub fn track(&self) -> Option<&TrackDto> {
        self.report.as_ref()?.track.as_ref()
    }

    /// 被控端手上那个队列在服务端的 id。
    ///
    /// `None` 有两种意思,调用方要分得开:一条上报都没收到过,或者被控端
    /// 那个队列**还没同步到服务端**(服务端不可达时本机照常起播)。
    /// 前一种由 [`Self::is_known`] 答,这里只答后一种。
    pub fn queue_id(&self) -> Option<i64> {
        self.report.as_ref()?.queue_id
    }

    /// 服务端上已提交的那一版(被控端所知)。
    pub fn revision(&self) -> Option<i64> {
        self.report.as_ref()?.revision
    }

    /// 被控端**实际应用**的那一版。
    pub fn applied_revision(&self) -> Option<i64> {
        self.report.as_ref()?.applied_revision
    }

    /// 服务端上有一版,而被控端还没应用上。
    ///
    /// 界面据此标「新版本待应用」——**数据库里写了不等于音箱在响**
    /// (`docs/adr/0031` 一)。两者都没有时不算待应用:那是「还没开始」,
    /// 不是「卡住了」。
    pub fn has_pending_revision(&self) -> bool {
        match (self.revision(), self.applied_revision()) {
            (Some(wanted), Some(applied)) => {
                wanted > applied
            }
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// 正在放的是队列里的哪一条。
    pub fn entry_id(&self) -> Option<i64> {
        self.report.as_ref()?.entry_id
    }

    /// 队列一共几首。拉到列表之前先拿它画「共 N 首」。
    pub fn queue_len(&self) -> u32 {
        self.report
            .as_ref()
            .map_or(0, |report| report.queue_len)
    }

    /// 被控端的音量。没有上报时按满音量算 —— 滑块总得停在某处。
    pub fn volume(&self) -> f32 {
        self.report
            .as_ref()
            .map_or(1.0, |report| report.volume)
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn device() -> DeviceDto {
        DeviceDto {
            id: "pc1".to_owned(),
            name: "pc1".to_owned(),
        }
    }

    fn track() -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "歌".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 200_000,
        }
    }

    /// 同一次执行会话里的第 `state_seq` 条上报。
    fn report(
        position_ms: u64,
        state: RemotePlayState,
        state_seq: u64,
    ) -> RemoteStateDto {
        report_from(EPOCH, position_ms, state, state_seq)
    }

    /// 这台被控端这一次启动的标识。跨重启才会变的那个数。
    const EPOCH: i64 = 1_700_000_000_000;

    fn report_from(
        epoch: i64,
        position_ms: u64,
        state: RemotePlayState,
        state_seq: u64,
    ) -> RemoteStateDto {
        RemoteStateDto {
            track: Some(track()),
            position_ms,
            state,
            volume: 0.7,
            queue_id: Some(7),
            revision: Some(3),
            applied_revision: Some(3),
            entry_id: Some(12),
            queue_len: 1,
            epoch,
            state_seq,
        }
    }

    /// 默认是本机:不选设备时一切照旧,没有第二个模式开关。
    #[test]
    fn output_defaults_to_local() {
        assert_eq!(Output::default(), Output::Local);
        assert_eq!(Output::default().target(), None);
    }

    /// 选了设备之后,那台设备的 id 就是命令要发去的地址。
    #[test]
    fn a_remote_output_names_its_device() {
        let output = Output::Remote(device());

        assert_eq!(output.target(), Some("pc1"));
    }

    /// 两次上报之间按本地时钟插值。
    ///
    /// 不插的话进度条每秒跳一格 —— 而被控端明明在连续地放。
    #[test]
    fn a_playing_report_is_interpolated_between_reports() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Playing, 1),
            50_000,
        );

        assert_eq!(view.position_ms(50_400), 10_400);
    }

    /// 缓冲时不插值:那一刻进度并没有在走。
    ///
    /// 照插的话手机上的进度条会自己往前爬,然后在下一次上报时跳回去 ——
    /// 而用户看到的是「卡了一下又倒回去」,比停着更像故障。
    #[test]
    fn a_buffering_report_is_not_interpolated() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Buffering, 1),
            50_000,
        );

        assert_eq!(view.position_ms(50_900), 10_000);
    }

    /// 暂停同理。
    #[test]
    fn a_paused_report_is_not_interpolated() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Paused, 1),
            50_000,
        );

        assert_eq!(view.position_ms(52_000), 10_000);
    }

    /// 插值不许越过这首歌的长度。
    ///
    /// 越过了就会显示 3:25 / 3:20,而那种数字一出现,用户就再也不信这条进度条。
    #[test]
    fn interpolation_stops_at_the_end_of_the_track() {
        let mut view = RemoteView::default();
        view.accept(
            report(199_000, RemotePlayState::Playing, 1),
            50_000,
        );

        assert_eq!(view.position_ms(52_000), 200_000);
    }

    /// 连着三秒收不到上报就算过期。
    ///
    /// 过期期间**不自动切回本机**(产品规则):切回去的话,用户一低头
    /// 就发现歌从手机里放出来了,而他要的是让 pc1 放。
    #[test]
    fn a_view_goes_stale_after_three_silent_seconds() {
        let mut view = RemoteView::default();
        view.accept(
            report(0, RemotePlayState::Playing, 1),
            50_000,
        );

        assert!(!view.is_stale(52_900));
        assert!(view.is_stale(53_100));
    }

    /// 十五秒收不到上报就算失联 —— 比过期晚得多,中间那段留给网络抖动。
    ///
    /// 两档必须分开:三秒就切回本机的话,一次正常的抖动就把声音从 pc1
    /// 抢回手机里;而只有过期这一档的话,撤权一丢遥控器就永久停在那儿(#102 F-003)。
    #[test]
    fn a_view_is_only_lost_long_after_it_goes_stale() {
        let mut view = RemoteView::default();
        view.accept(
            report(0, RemotePlayState::Playing, 1),
            50_000,
        );

        assert!(view.is_stale(55_000), "五秒早就过期了");
        assert!(
            !view.is_lost(55_000),
            "但还不算失联 —— 抖一下不该把人踢回本机"
        );
        assert!(!view.is_lost(64_900));
        assert!(view.is_lost(65_100));
    }

    /// 一条上报都没收到过时不算失联,那是「还没开始」。
    ///
    /// 算的话,接管后头十五秒里但凡快照慢一点,就会被自己判死回本机。
    #[test]
    fn a_view_without_any_report_is_never_lost() {
        assert!(!RemoteView::default().is_lost(u64::MAX));
    }

    /// 过期之后停止插值:那条进度条已经不知道真相了,别让它接着爬。
    #[test]
    fn a_stale_view_stops_interpolating() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Playing, 1),
            50_000,
        );

        assert_eq!(view.position_ms(60_000), 13_000);
    }

    /// 一条上报都还没收到时是「还不知道」,**不是**过期。
    ///
    /// 两者该导向相反的行为,所以不能合成一个判断:刚接管、快照还在路上的
    /// 那几百毫秒里控制要能发出去(选完设备马上点歌是最自然的操作顺序);
    /// 真的过期时才该挡住。此前这里写的是「没有上报也算过期」,那会让
    /// 选完设备的第一次点击被静默丢掉。
    #[test]
    fn a_view_without_any_report_is_unknown_not_stale() {
        let view = RemoteView::default();

        assert!(!view.is_known());
        assert!(!view.is_stale(0));
        assert_eq!(view.state(), RemotePlayState::Idle);
        assert_eq!(view.track(), None);
    }

    /// 新的上报顶掉旧的,并且把过期计时重新起头。
    #[test]
    fn a_fresh_report_replaces_the_old_one() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Playing, 1),
            50_000,
        );

        let accepted = view.accept(
            report(20_000, RemotePlayState::Paused, 2),
            54_000,
        );

        assert!(accepted);
        assert_eq!(view.position_ms(54_500), 20_000);
        assert!(!view.is_stale(54_500));
    }

    /// 迟到的旧上报丢掉。
    ///
    /// 重连之后旧连接上的残余可能后到,收下它进度条就会倒退一次 ——
    /// 而那看起来正好像是「拖进度失败了」。
    #[test]
    fn an_out_of_order_report_is_ignored() {
        let mut view = RemoteView::default();
        view.accept(
            report(20_000, RemotePlayState::Playing, 9),
            50_000,
        );

        let accepted = view.accept(
            report(10_000, RemotePlayState::Playing, 5),
            50_100,
        );

        assert!(!accepted, "旧上报不该被收下");
        assert_eq!(view.position_ms(50_000), 20_000);
    }

    /// 被控端重启之后,序号从头数,而那一条仍然是**新**的。
    ///
    /// 只比 `state_seq` 的话,重启后的第 1 条永远排在重启前的第 900 条后面,
    /// 于是遥控器把新进程报来的一切全丢掉 —— 界面停在旧状态上,而它看起来
    /// 就是「对面没反应」。`epoch` 是跨重启那一维,少了它这条链路排不了序,
    /// 而这正是从前那个挂钟 `sent_at` 唯一还算管用的地方。
    #[test]
    fn a_report_from_a_newer_epoch_wins_despite_a_lower_seq()
     {
        let mut view = RemoteView::default();
        view.accept(
            report(20_000, RemotePlayState::Playing, 900),
            50_000,
        );

        let restarted = view.accept(
            report_from(
                EPOCH + 1,
                0,
                RemotePlayState::Playing,
                1,
            ),
            50_100,
        );

        assert!(
            restarted,
            "新一次执行会话的第一条该被收下"
        );
        assert_eq!(view.position_ms(50_100), 0);
    }

    /// 反过来,旧 epoch 的残余无论序号多大都丢掉。
    #[test]
    fn a_report_from_an_older_epoch_is_ignored() {
        let mut view = RemoteView::default();
        view.accept(
            report_from(
                EPOCH + 1,
                20_000,
                RemotePlayState::Playing,
                1,
            ),
            50_000,
        );

        let stale = view.accept(
            report(0, RemotePlayState::Playing, 900),
            50_100,
        );

        assert!(!stale, "上一次执行会话的残余不该被收下");
        assert_eq!(view.position_ms(50_000), 20_000);
    }

    /// 队列的**标识**、音量与曲目都从上报里读。
    ///
    /// 队列本身不在上报里了(`docs/adr/0031`):遥控器按这几个标识去 HTTP 上
    /// 取,取到的是只读显示缓存,不是第二份真相。
    #[test]
    fn the_view_mirrors_the_queue_identity_and_volume() {
        let mut view = RemoteView::default();
        view.accept(
            report(0, RemotePlayState::Playing, 1),
            0,
        );

        assert_eq!(view.queue_id(), Some(7));
        assert_eq!(view.revision(), Some(3));
        assert_eq!(view.applied_revision(), Some(3));
        assert_eq!(view.entry_id(), Some(12));
        assert_eq!(view.queue_len(), 1);
        assert_eq!(view.volume(), 0.7);
        assert_eq!(view.track(), Some(&track()));
    }

    /// 服务端那一版比被控端应用的新 —— 界面该标「新版本待应用」。
    ///
    /// 不标的话,用户点了歌、库里也写下了,而音箱还在放上一首,
    /// 界面却一切正常 —— 那正是三层裁决要防的那种错。
    #[test]
    fn a_revision_ahead_of_the_applied_one_is_pending() {
        let mut view = RemoteView::default();
        let mut ahead =
            report(0, RemotePlayState::Playing, 1);
        ahead.revision = Some(5);
        ahead.applied_revision = Some(3);
        view.accept(ahead, 0);

        assert!(view.has_pending_revision());
    }

    /// 两版对齐就不是待应用。
    #[test]
    fn a_report_caught_up_with_the_server_is_not_pending() {
        let mut view = RemoteView::default();
        view.accept(
            report(0, RemotePlayState::Playing, 1),
            0,
        );

        assert!(!view.has_pending_revision());
    }

    /// 切回本机时把镜像清掉。
    ///
    /// 留着的话,下一次接管另一台设备会先闪一眼上一台的歌名。
    #[test]
    fn clearing_forgets_the_last_report() {
        let mut view = RemoteView::default();
        view.accept(
            report(10_000, RemotePlayState::Playing, 1),
            50_000,
        );

        view.clear();

        assert_eq!(view.track(), None);
        assert!(!view.is_known());
    }
}
