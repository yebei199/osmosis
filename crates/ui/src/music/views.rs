//! 浏览视图各存各的(#137 ④):每日推荐、最近播放、某个歌单、某位歌手、某次搜索,
//! 每一个都有自己那份曲目,互不覆盖。
//!
//! 以前它们共用一个槽,「新来源整批替换」—— 于是进「我喜欢的」先看到的是每日推荐,
//! 晚到的推荐还能把已经摆好的红心列表顶掉。现在列表上摆的永远是**当前视图**那一份:
//! 切视图同一步就摆它的内存缓存(没有就是加载态),响应只写回发起它的那个视图,
//! 而且只有视图与代号都还对得上时才投影到屏幕上。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use app_core::TracksDto;

/// 内存里最多留几个视图。超了先扔最久没进过的那个(当前那个永远不扔)。
///
/// 一份是几百到上千行 `TrackDto`,十六份是几 MB,够覆盖「来回翻几个歌单」。
const CAPACITY: usize = 16;

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

/// 视图键:谁的账号下的哪个来源。换了账号,同一个「每日推荐」也是另一个视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewKey {
    account: u64,
    source: ViewSource,
}

/// 一次请求的凭据:它替哪个视图取、是那个视图的第几次取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ticket {
    key: ViewKey,
    generation: u64,
}

impl Ticket {
    pub(crate) fn source(&self) -> &ViewSource {
        &self.key.source
    }
}

/// 一个视图此刻的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// 有一次取数在路上。
    Loading,
    Ready,
    /// 最近一次取数失败了。手上若还有上次那份,照样摆着。
    Failed,
}

/// 切到一个视图时该摆什么:它手上那份(可能没有),以及是不是还在取。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shown {
    pub(crate) tracks: Option<TracksDto>,
    pub(crate) loading: bool,
}

/// 一份响应落到了哪。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Landing {
    /// 过期了(被同一视图更新的一次取数顶掉,或者换了账号):扔掉。
    Dropped,
    /// 进了自己的缓存,但用户已经不在这个视图上了。
    Stored,
    /// 进了缓存,而且它就是当前视图 —— 该投影到屏幕上。
    Current,
}

struct Entry {
    key: ViewKey,
    tracks: Option<TracksDto>,
    status: Status,
    /// 最近一次为它发出的取数的代号。只有这一次的响应算数。
    generation: u64,
}

impl Entry {
    fn shown(&self) -> Shown {
        Shown {
            tracks: self.tracks.clone(),
            loading: self.tracks.is_none()
                && self.status == Status::Loading,
        }
    }
}

#[derive(Default)]
struct State {
    current: Option<ViewKey>,
    /// 全局递增的代号。每个视图各记自己最近那一次,但号不复用 ——
    /// A→B→A 时第二次进 A 拿到的号一定比第一次大。
    generation: u64,
    /// 最久没进过的在前。
    entries: VecDeque<Entry>,
}

impl State {
    /// 换了账号:上一个人的视图全部扔掉,当前视图也作废。
    fn forget_other_accounts(&mut self, account: u64) {
        self.entries.retain(|entry| entry.key.account == account);
        if self
            .current
            .as_ref()
            .is_some_and(|key| key.account != account)
        {
            self.current = None;
        }
    }

    fn find(&self, key: &ViewKey) -> Option<usize> {
        self.entries.iter().position(|entry| &entry.key == key)
    }

    /// 取出(没有就新建)这个视图,挪到「最近进过」那一端。
    fn touch(&mut self, key: ViewKey) -> &mut Entry {
        let entry = match self.find(&key) {
            Some(index) => self
                .entries
                .remove(index)
                .expect("刚找到的下标"),
            None => Entry {
                key,
                tracks: None,
                status: Status::Ready,
                generation: 0,
            },
        };
        self.entries.push_back(entry);
        self.evict();
        self.entries.back_mut().expect("刚放进去的那一个")
    }

    /// 超了容量就从最久没进过的那头扔,跳过当前视图。
    fn evict(&mut self) {
        while self.entries.len() > CAPACITY {
            let Some(index) = self.entries.iter().position(|entry| {
                Some(&entry.key) != self.current.as_ref()
            }) else {
                return;
            };
            self.entries.remove(index);
        }
    }

    /// 这张凭据还算不算数:账号没换、而且是它那个视图最近的一次。
    fn live(&mut self, ticket: &Ticket, account: u64) -> Option<&mut Entry> {
        self.forget_other_accounts(account);
        if ticket.key.account != account {
            return None;
        }
        let index = self.find(&ticket.key)?;
        let entry = &mut self.entries[index];
        (entry.generation == ticket.generation).then_some(entry)
    }
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
    state: Rc<RefCell<State>>,
    /// 读当前账号。平时读会话;测试换成自己的,好模拟登出与换号。
    account: Rc<RefCell<Rc<dyn Fn() -> u64>>>,
}

impl Default for Views {
    fn default() -> Self {
        Self {
            state: Rc::default(),
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

    /// 进一个视图,并为它发一次取数:它成为当前视图,拿到一个新代号。
    ///
    /// 同一个视图此前在路上的那次随之作废 —— 它的响应回来时代号对不上。
    pub(crate) fn begin(&self, source: ViewSource) -> (Ticket, Shown) {
        let account = self.account();
        let mut state = self.state.borrow_mut();
        state.forget_other_accounts(account);
        state.generation += 1;
        let generation = state.generation;
        let key = ViewKey { account, source };
        state.current = Some(key.clone());
        let entry = state.touch(key.clone());
        entry.generation = generation;
        entry.status = Status::Loading;
        (Ticket { key, generation }, entry.shown())
    }

    /// 只切到一个视图,不取:摆它手上那份。它若还有一次取数在路上,那一次照样算数。
    pub(crate) fn show(&self, source: ViewSource) -> Shown {
        let account = self.account();
        let mut state = self.state.borrow_mut();
        state.forget_other_accounts(account);
        let key = ViewKey { account, source };
        state.current = Some(key.clone());
        state.touch(key).shown()
    }

    /// 离开所有视图:当前没有任何一批歌可摆(比如还没搜过的搜索页)。
    pub(crate) fn leave(&self) {
        self.state.borrow_mut().current = None;
    }

    /// 最近进过的那次搜索。回到搜索页时摆它。
    pub(crate) fn latest_search(&self) -> Option<ViewSource> {
        let account = self.account();
        self.state
            .borrow()
            .entries
            .iter()
            .rev()
            .map(|entry| &entry.key)
            .find(|key| {
                key.account == account
                    && matches!(key.source, ViewSource::Search(_))
            })
            .map(|key| key.source.clone())
    }

    /// 一份曲目回来了(落盘缓存或网络)。凭据还算数就写进它自己的视图。
    pub(crate) fn accept(
        &self,
        ticket: &Ticket,
        tracks: TracksDto,
        settled: bool,
    ) -> Landing {
        let account = self.account();
        let mut state = self.state.borrow_mut();
        let Some(entry) = state.live(ticket, account) else {
            return Landing::Dropped;
        };
        entry.tracks = Some(tracks);
        if settled {
            entry.status = Status::Ready;
        }
        Self::landing(&state, ticket)
    }

    /// 这次取数失败了。返回它落在哪 —— 只有落在当前视图上才值得告诉用户。
    pub(crate) fn fail(&self, ticket: &Ticket) -> Landing {
        let account = self.account();
        let mut state = self.state.borrow_mut();
        let Some(entry) = state.live(ticket, account) else {
            return Landing::Dropped;
        };
        entry.status = Status::Failed;
        Self::landing(&state, ticket)
    }

    /// 当前视图此刻该摆什么。
    pub(crate) fn current(&self) -> Option<Shown> {
        let state = self.state.borrow();
        let key = state.current.as_ref()?;
        state
            .entries
            .iter()
            .find(|entry| &entry.key == key)
            .map(Entry::shown)
    }

    fn landing(state: &State, ticket: &Ticket) -> Landing {
        if state.current.as_ref() == Some(&ticket.key) {
            Landing::Current
        } else {
            Landing::Stored
        }
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
