use similar_asserts::assert_eq;

use super::*;

/// 错误链要一路摊到底,不能停在最外面那句笼统话上。
///
/// 「Symphonia decoder returned an error」对着日志的人等于没说 ——
/// 得说出是"没有时基"还是"这条流不能回跳",两者的修法完全不同。
#[test]
fn a_cause_chain_is_spelled_out_to_the_end() {
    /// 最里面那一层:真正说明问题的那句。
    #[derive(Debug)]
    struct Why;
    impl core::fmt::Display for Why {
        fn fmt(
            &self,
            f: &mut core::fmt::Formatter<'_>,
        ) -> core::fmt::Result {
            write!(f, "这条流不能回跳")
        }
    }
    impl core::error::Error for Why {}

    /// 外面那一层:笼统,单看等于没说 —— rodio 的
    /// `SeekError::SymphoniaDecoder` 正是这个形状。
    #[derive(Debug)]
    struct Vague(Why);
    impl core::fmt::Display for Vague {
        fn fmt(
            &self,
            f: &mut core::fmt::Formatter<'_>,
        ) -> core::fmt::Result {
            write!(f, "解码器报错了")
        }
    }
    impl core::error::Error for Vague {
        fn source(
            &self,
        ) -> Option<&(dyn core::error::Error + 'static)>
        {
            Some(&self.0)
        }
    }

    assert_eq!(
        full_cause(&Vague(Why)),
        "解码器报错了: 这条流不能回跳"
    );
    assert_eq!(
        full_cause(&Why),
        "这条流不能回跳",
        "只有一层时不该多出一个尾巴"
    );
}
