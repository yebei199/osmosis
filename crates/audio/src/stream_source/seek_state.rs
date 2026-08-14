//! 跳转的状态机:一次跳转从发出到有结论之间,界面要能问出它走到哪了。

use std::sync::{Arc, Mutex};

/// 跳转走到哪一步了。
///
/// 跳转是**异步**的:请求送出去就返回,真正的读字节发生在解码线程上,可能
/// 要好几秒(跳到还没下到的位置要重开一个 range 请求)。所以"成没成"不能
/// 由 `try_seek` 的返回值回答 —— 那时还没有答案。界面改为定期问这里。
#[derive(Clone, Default)]
pub struct SeekState(Arc<Mutex<Phase>>);

/// [`SeekState`] 的三种样子。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// 没有跳转在路上。
    #[default]
    Idle,
    /// 请求已经送出,解码线程还在取字节。
    Seeking,
    /// 上一次跳转失败了,附带原因。取走即清 —— 一句提示说一次就够。
    Failed(String),
}

impl SeekState {
    /// 还在取字节吗。界面据此显示「缓冲中」。
    pub(super) fn begin(&self) {
        self.set(Phase::Seeking);
    }

    pub(super) fn finish(&self) {
        self.set(Phase::Idle);
    }

    pub(super) fn fail(&self, why: String) {
        self.set(Phase::Failed(why));
    }

    fn set(&self, phase: Phase) {
        // 锁只护一个枚举,中毒了也没有半截状态可言,取回去接着用
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) =
            phase;
    }

    /// 还在等字节吗。界面据此显示「缓冲中」。
    pub fn is_seeking(&self) -> bool {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
            == Phase::Seeking
    }

    /// 取走上一次失败的原因,取走即清。
    ///
    /// 清掉是必须的:界面每秒问一次,不清的话那句"这首跳不了"会一直重贴,
    /// 把后面真正该说的话盖住。
    pub fn take_failure(&self) -> Option<String> {
        let mut phase = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Phase::Failed(why) = &*phase else {
            return None;
        };
        let why = why.clone();
        *phase = Phase::Idle;
        Some(why)
    }
}

#[cfg(test)]
mod tests;
