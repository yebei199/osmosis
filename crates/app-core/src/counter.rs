//! 点击计数器。

/// 点击计数器。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counter(i32);

impl Counter {
    /// 当前计数值。
    pub fn value(self) -> i32 {
        self.0
    }

    /// 计数加一。
    ///
    /// 饱和到 [`i32::MAX`]:计数器溢出不是一种需要让应用崩溃的情况,
    /// 而且 debug profile 下的整数溢出会 panic,release 下会静默回绕成负数 ——
    /// 两种行为都不可接受,所以这里显式选择饱和。
    pub fn bump(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新建的计数器从 0 开始。
    #[test]
    fn 新建计数器为零() {
        assert_eq!(Counter::default().value(), 0);
    }

    /// 每次 bump 让计数值加一。
    #[test]
    fn bump_使计数值加一() {
        let mut counter = Counter::default();
        counter.bump();
        assert_eq!(counter.value(), 1);
        counter.bump();
        assert_eq!(counter.value(), 2);
    }

    /// 边界:计数值到达 i32::MAX 后继续 bump 应当饱和,而不是 panic 或回绕。
    #[test]
    fn bump_在最大值处饱和() {
        let mut counter = Counter(i32::MAX);
        counter.bump();
        assert_eq!(counter.value(), i32::MAX);
    }
}
