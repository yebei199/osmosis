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

/// 一次操作做完之后要捎给服务端的那句话。
///
/// 与报告同一条请求发出去:播放端知道「我到哪了」与「那次操作成没成」是同一刻
/// 的事,分两次发会出现两者互相矛盾的中间态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::music) struct Outcome {
    pub(in crate::music) operation_id: String,
    pub(in crate::music) applied: bool,
    pub(in crate::music) reason: Option<String>,
}

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
    pending: Option<Outcome>,
    /// 上一次**报出去**的播放次序。
    ///
    /// 留着它才判得出「这一次要不要带排列」:排列只在洗牌或回卷改变它的时候
    /// 才同步,每秒都带的话服务端那边就是每秒重写几千个 bigint,而线上字节数
    /// 并不会涨 —— 一个 AC-2 抓不到的写放大(`docs/adr/0031` 六)。
    reported_order: Vec<i64>,
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

    /// 服务端上出现了一版我们还没应用的。
    pub(in crate::music) fn want(
        &self,
        queue_id: i64,
        revision: i64,
    ) {
        let mut state = self.inner.borrow_mut();
        // 换了个队列就等于换了一批:旧的对应关系一条都不作数了。
        if state.queue_id != Some(queue_id) {
            state.queue_id = Some(queue_id);
            state.applied_revision = None;
            state.entry_ids.clear();
        }
        state.desired_revision = Some(revision);
    }

    /// 这一批是本机自己攒的,还没同步到服务端去。
    ///
    /// 不是错误状态:服务端不可达时本机照常起播,界面标一句「未同步」即可。
    pub(in crate::music) fn detach(&self) {
        *self.inner.borrow_mut() = State::default();
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

    /// 记下一次操作的下场,等下一条报告捎走。
    pub(in crate::music) fn note(&self, outcome: Outcome) {
        self.inner.borrow_mut().pending = Some(outcome);
    }

    /// 取走那句话 —— **只捎一次**。
    ///
    /// 留着的话,每秒那条报告会把同一次操作的下场反复汇报,而服务端每收到
    /// 一次就按 `operation_id` 去改一次意图的状态。
    pub(in crate::music) fn take_outcome(
        &self,
    ) -> Option<Outcome> {
        self.inner.borrow_mut().pending.take()
    }
}

#[cfg(test)]
mod tests;
