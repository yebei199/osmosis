//! 执行副本的身份:手上这批歌是服务端哪个队列的哪一版,每一条又是哪个条目。
//!
//! 队列本身仍然住在 `app_core::Queue` 里 —— 那一份管的是「放哪一首、下一首是
//! 谁」,一个字节都不必知道服务端的存在(`docs/adr/0031` 四:自动下一首不等
//! 服务端批准)。这里存的是**它与服务端那一版的对应关系**,与它并排放着,
//! 换一批时一起换。
//!
//! 三件事只有这里说得清:
//!
//! - **`applied` 与 `desired` 必须分开。** 两者不等就是「新版本待应用」,界面
//!   要标出来。合成一个的话,下载失败时只能在「谎报已应用」与「谎报没收到」
//!   之间挑一个(`docs/adr/0031` 一)。
//! - **`entry_id` 不是下标。** 队列允许同一首歌出现多次,下标随插入删除整体
//!   挪位,而上报里要说的是「正在放的是哪一条」。
//! - **没有 `queue_id` 是一种正常状态**,不是错误:服务端不可达时本机照常
//!   起播,只是这一份还没同步上去(`docs/adr/0031` 八)。

use std::cell::RefCell;
use std::rc::Rc;

/// 两次补同步之间至少隔多久。
///
/// 服务端不可达时每秒打一发,日志会被刷满,而它恢复的时刻不由我们决定;
/// 隔太久又会让用户在服务端回来之后还盯着「未同步」很久。半分钟是拍的
// ponytail: 真要紧的话该由重连事件触发,而不是轮询问一句
const RESYNC_EVERY_MS: u64 = 30_000;

/// 执行副本与服务端那一版的对应关系。
#[derive(Clone, Default)]
pub(in crate::music) struct Execution {
    inner: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    queue_id: Option<i64>,
    applied_revision: Option<i64>,
    desired_revision: Option<i64>,
    /// 与 `Queue::tracks()` 一一对应的 `entry_id`。
    ///
    /// 平行数组而不是把 id 塞进 `TrackDto`:那个类型是**曲目**,而 `entry_id`
    /// 说的是「这一批里的第几条」—— 同一首歌在队列里出现两次,曲目是同一个,
    /// 条目是两个。
    entry_ids: Vec<i64>,
    /// 上一次试着把本机队列同步上去是什么时候。
    last_sync_try_ms: u64,
    /// 上一次**报出去**的播放次序。
    ///
    /// 留着它才判得出「这一次要不要带排列」:排列只在洗牌或回卷改变它的时候
    /// 才同步,每秒都带的话服务端那边就是每秒重写几千个 bigint,而线上字节数
    /// 并不会涨 —— 一个 AC-2 抓不到的写放大(`docs/adr/0031` 六)。
    reported_order: Vec<i64>,
    /// 发起过几次本机队列发布。与补同步那只钟一样不随 `detach` 清零。
    publishes: u64,
    /// 本进程报过几条执行报告,与 [`epoch`] 一起组成报告的顺序键。不随 `detach` 清零。
    state_seq: u64,
}

/// 这一次执行会话的标识:进程启动后第一次报告时的毫秒挂钟。与服务端
/// `play_queue_reports.epoch` 是同一个数,跨重启比大小要靠它。
// ponytail: 挂钟倒退时新进程会拿到更小的 epoch,那一次的报告会被服务端全丢掉。
// 真出现再换单调时钟加持久计数器
fn epoch() -> i64 {
    static EPOCH: std::sync::OnceLock<i64> =
        std::sync::OnceLock::new();
    *EPOCH
        .get_or_init(|| crate::sync::group::now_ms() as i64)
}

impl Execution {
    /// 换上一份新的执行副本:这是服务端 `queue_id` 的第 `revision` 版。
    ///
    /// **调用方要先把曲目真的换好**,这里只记账。两步之间不留空档是调用方的
    /// 事(见 `dispatch::adopt_remote_queue`)。
    pub(in crate::music) fn adopt(
        &self,
        queue_id: i64,
        revision: i64,
        entry_ids: Vec<i64>,
    ) {
        let mut state = self.inner.borrow_mut();
        state.queue_id = Some(queue_id);
        state.applied_revision = Some(revision);
        state.desired_revision = Some(revision);
        state.entry_ids = entry_ids;
    }

    /// 这一批是本机自己攒的,还没同步到服务端去。
    ///
    /// 不是错误状态:服务端不可达时本机照常起播,界面标一句「未同步」即可。
    ///
    /// **补同步那只钟不跟着清。** 它记的是「上一次试是什么时候」,与
    /// 「手上这份是服务端哪一版」无关。清掉的话下一轮又到点,
    /// [`RESYNC_EVERY_MS`] 那道节流就形同虚设 —— 而发布失败正好走这里,
    /// 于是服务端不可达时反倒变成每秒打一发(2026-09-21 小米 13 上量到)。
    pub(in crate::music) fn detach(&self) {
        let mut state = self.inner.borrow_mut();
        let (last_sync_try_ms, publishes, state_seq) = (
            state.last_sync_try_ms,
            state.publishes,
            state.state_seq,
        );
        *state = State::default();
        state.last_sync_try_ms = last_sync_try_ms;
        state.publishes = publishes;
        state.state_seq = state_seq;
    }

    /// 一次本机队列发布发出去了。
    ///
    /// **补同步那只钟跟着拨到现在**:发布在路上的那几百毫秒里 `queue_id`
    /// 还是空的,每秒那一趟 tick 问 [`Self::due_for_resync`] 就会以为这一批
    /// 没同步过,紧跟着再 `POST /queues` 一次(#125)。
    // ponytail: 一次发布最多两次请求 × 10 秒超时,小于 RESYNC_EVERY_MS;
    // 哪天超时加长到逼近它,就该换成显式的「在途」标记。
    pub(in crate::music) fn note_publish(
        &self,
        now_ms: u64,
    ) {
        let mut state = self.inner.borrow_mut();
        state.last_sync_try_ms = now_ms;
        state.publishes += 1;
    }

    /// 发起过几次本机队列发布。
    #[cfg(test)]
    pub(in crate::music) fn publishes(&self) -> u64 {
        self.inner.borrow().publishes
    }

    /// 手上这份的身份,报给遥控它的那台设备。
    pub(in crate::music) fn identity(
        &self,
    ) -> (Option<i64>, Option<i64>, Option<i64>) {
        let state = self.inner.borrow();
        (
            state.queue_id,
            state.desired_revision,
            state.applied_revision,
        )
    }

    /// 正在放的那一条是哪个 `entry_id`。
    ///
    /// 越界给 `None` 而不是 0:0 会被读成「第一条」,而那是一句谎话。
    pub(in crate::music) fn entry_at(
        &self,
        index: usize,
    ) -> Option<i64> {
        self.inner.borrow().entry_ids.get(index).copied()
    }

    /// 条目 `entry_id` 在手上这份里是第几首。不在就是 `None`。
    pub(in crate::music) fn index_of(
        &self,
        entry_id: i64,
    ) -> Option<usize> {
        self.inner
            .borrow()
            .entry_ids
            .iter()
            .position(|id| *id == entry_id)
    }

    /// 这一次要报的排列:与上次报过的一样就给 `None`,让服务端沿用。
    ///
    /// 问过就算数 —— 它同时是「已经报到哪一版排列」的那份记录。
    pub(in crate::music) fn order_to_report(
        &self,
        order: &[i64],
    ) -> Option<Vec<i64>> {
        let mut state = self.inner.borrow_mut();
        if state.reported_order == order {
            return None;
        }
        state.reported_order = order.to_vec();
        Some(state.reported_order.clone())
    }

    /// 该不该再试一次把本机队列同步上去(AC-12 的「恢复后对账」)。
    ///
    /// 问过就算数 —— 它同时记下「这一次问是什么时候」,所以不会每秒都试。
    /// 服务端不可达时每秒打一发,日志会被刷满,而它恢复的时刻不由我们决定。
    pub(in crate::music) fn due_for_resync(
        &self,
        now_ms: u64,
    ) -> bool {
        let mut state = self.inner.borrow_mut();
        // 已经有 id 了就没什么可对的。
        if state.queue_id.is_some() {
            return false;
        }
        if now_ms.saturating_sub(state.last_sync_try_ms)
            < RESYNC_EVERY_MS
        {
            return false;
        }
        state.last_sync_try_ms = now_ms;
        true
    }

    /// 这一条报告的顺序键:`(epoch, state_seq)`,序号取完即加一。
    pub(in crate::music) fn stamp(&self) -> (i64, u64) {
        let mut state = self.inner.borrow_mut();
        state.state_seq += 1;
        (epoch(), state.state_seq)
    }
}

#[cfg(test)]
mod tests;
