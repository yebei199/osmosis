//! 浏览视图各存各的(#137 ④):每日推荐、最近播放、某个歌单、某位歌手、某次搜索,
//! 每一个都有自己那份曲目,互不覆盖。
//!
//! 以前它们共用一个槽,「新来源整批替换」—— 于是进「我喜欢的」先看到的是每日推荐,
//! 晚到的推荐还能把已经摆好的红心列表顶掉。现在列表上摆的永远是**当前视图**那一份:
//! 切视图同一步就摆它的内存缓存(没有就是加载态),响应只写回发起它的那个视图,
//! 而且只有视图与代号都还对得上时才投影到屏幕上。

use std::cell::RefCell;
use std::rc::Rc;

/// 一个浏览视图是什么:来源连同它的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewSource {
    Daily,
    Recent,
    /// 歌单:来源与歌单 id。
    Playlist(crate::library::playlist::Source, String),
    Artist(String),
    Search(String),
}

/// 当前账号的指纹。token 本身不存进键里,存它的哈希 —— 够分辨账号,
/// 又不在内存里多留一份凭据。没登录是 0。
fn current_account() -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault};
    api::session::token().map_or(0, |token| {
        BuildHasherDefault::<std::hash::DefaultHasher>::default()
            .hash_one(token)
    })
}

/// 全部视图的内存缓存与「现在摆的是哪一个」。
#[derive(Clone)]
pub(crate) struct Views {
    /// 读当前账号。平时读会话;测试换成自己的,好模拟登出与换号。
    account: Rc<RefCell<Rc<dyn Fn() -> u64>>>,
}

impl Default for Views {
    fn default() -> Self {
        Self {
            account: Rc::new(RefCell::new(Rc::new(
                current_account,
            ))),
        }
    }
}

impl Views {
    /// 此刻是谁的账号。
    pub(crate) fn account(&self) -> u64 {
        let read = self.account.borrow().clone();
        read()
    }

    /// 换掉读账号的办法(测试用)。
    #[cfg(test)]
    pub(crate) fn set_account(
        &self,
        read: impl Fn() -> u64 + 'static,
    ) {
        *self.account.borrow_mut() = Rc::new(read);
    }
}
