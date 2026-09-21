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
    /// 正在取数的那一次操作。
    ///
    /// 取数是一次 HTTP 往返,几百毫秒到几秒。这期间可能发生三件事,而它们
    /// 都该让这一次作废(`docs/adr/0031` 七):遥控器又点了一首(新操作顶掉
    /// 旧的)、本机失权、用户退出被控。少了它,那几秒之后到货的旧队列会把
    /// 已经换上的新队列盖掉 —— 而用户看到的是「点了 B,放出来的是 A」。
    in_flight: Option<String>,
    /// 最后一次**应用成功**的操作。
    ///
    /// 重试同一次点播不该再次重置播放(`docs/adr/0031` 七):遥控器重发一遍
    /// 是常态(命令丢了、重连之后补一次),而重新取一遍队列、从头起播那一首,
    /// 在用户那里就是「歌自己跳回开头了」。
    applied_operation: Option<String>,
    /// 上一次试着把本机队列同步上去是什么时候。
    last_sync_try_ms: u64,
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

    /// 开始应用一次操作。**返回 `false` 表示这一次已经应用过了,别再动播放。**
    ///
    /// 幂等那一半:重试同一个 `operation_id` 不该再次重置播放。
    pub(in crate::music) fn begin(
        &self,
        operation_id: &str,
    ) -> bool {
        let mut state = self.inner.borrow_mut();
        if state.applied_operation.as_deref()
            == Some(operation_id)
        {
            return false;
        }
        state.in_flight = Some(operation_id.to_owned());
        true
    }

    /// 这一次取数回来时,它还算不算数。
    ///
    /// 不算数的三种情形共用这一个判据:被更新的操作顶掉、失权、退出被控 ——
    /// 后两种由 [`Self::abandon`] 清掉在途那一个。
    pub(in crate::music) fn still_current(
        &self,
        operation_id: &str,
    ) -> bool {
        self.inner.borrow().in_flight.as_deref()
            == Some(operation_id)
    }

    /// 在途那次操作作废:失权、换目标、退出被控。
    ///
    /// 只丢在途的那一个,**不动已经应用的那份副本** —— 失权不等于停止播放
    /// (`docs/adr/0030`:手机没电不能让 pc1 停)。
    pub(in crate::music) fn abandon(&self) {
        self.inner.borrow_mut().in_flight = None;
    }

    /// 记下一次操作的下场,等下一条报告捎走。
    ///
    /// 顺带给在途那一位收尾:成了就记住它(下次重试认得出来),没成就只是
    /// 清掉 —— 失败过的那一次**重试是应该的**。
    pub(in crate::music) fn note(&self, outcome: Outcome) {
        let mut state = self.inner.borrow_mut();
        if state.in_flight.as_deref()
            == Some(outcome.operation_id.as_str())
        {
            state.in_flight = None;
            if outcome.applied {
                state.applied_operation =
                    Some(outcome.operation_id.clone());
            }
        }
        state.pending = Some(outcome);
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
