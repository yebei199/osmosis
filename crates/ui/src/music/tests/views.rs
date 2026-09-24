//! 浏览视图各存各的(#137 ④,见 `music::views`)。
//!
//! 响应什么时候回来由测试说了算:每个请求拿一个 [`Late`],`send` 那一刻它才
//! 就绪。窗口是 pumped 的,唤醒当场推进协程 —— `send` 返回时响应已经处理完。

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::task::{Poll, Waker};

use similar_asserts::assert_eq;

use super::super::fixtures::*;
use super::super::*;
use super::{batch, shown_ids};
use crate::library::playlist::Source;

type Answer = Result<TracksDto, api::ApiError>;

/// 一个由测试决定何时回来的响应。
struct Late(Rc<RefCell<(Option<Answer>, Option<Waker>)>>);

impl Late {
    fn send(&self, answer: Answer) {
        let waker = {
            let mut slot = self.0.borrow_mut();
            slot.0 = Some(answer);
            slot.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

fn late() -> (Late, impl Future<Output = Answer>) {
    let slot = Rc::new(RefCell::new((None, None)));
    let waiting = slot.clone();
    let answer = std::future::poll_fn(move |cx| {
        let mut slot = waiting.borrow_mut();
        match slot.0.take() {
            Some(answer) => Poll::Ready(answer),
            None => {
                slot.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    });
    (Late(slot), answer)
}

/// 进一个视图:没有落盘缓存,网络那份等测试 `send`。
fn visit(
    ui: &MainWindow,
    deck: &Deck,
    source: ViewSource,
) -> Late {
    let (handle, answer) = late();
    fetch_cached_into(
        &ui.as_weak(),
        deck,
        crate::runtime::trace::Action::begin("test-view"),
        source,
        async { None },
        answer,
    );
    handle
}

fn liked() -> ViewSource {
    ViewSource::Playlist(Source::Liked, "liked".to_owned())
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

/// **现场那个症状**:推荐还在路上时进了「我喜欢的」,红心列表摆好之后推荐才回来。
/// 屏幕上必须还是红心那一批 —— 推荐只该进它自己那份缓存。
#[test]
fn a_late_daily_does_not_overwrite_liked() {
    let (ui, deck) = deck_window_pumped();

    let daily = visit(&ui, &deck, ViewSource::Daily);
    let liked_answer = visit(&ui, &deck, liked());
    liked_answer.send(Ok(batch(&["l1", "l2"])));
    daily.send(Ok(batch(&["d1"])));

    assert_eq!(
        shown_ids(&ui),
        ids(&["l1", "l2"]),
        "晚到的推荐顶掉了已经摆好的红心列表"
    );
}

/// 进一个从没取过的视图:同一步列表就空出来、标成加载中,上一个视图的歌一帧都不留。
#[test]
fn entering_a_new_view_shows_loading_not_the_previous_list() {
    let (ui, deck) = deck_window_pumped();
    visit(&ui, &deck, ViewSource::Daily)
        .send(Ok(batch(&["d1"])));

    let _pending = visit(&ui, &deck, liked());

    assert_eq!(
        shown_ids(&ui),
        Vec::<String>::new(),
        "「我喜欢的」还没回来,摆的却是推荐"
    );
    assert!(
        ui.global::<Player>().get_tracks_loading(),
        "没有缓存的视图该显示加载态"
    );
}

/// 切回取过的视图:同一步摆出它的内存缓存,不等网络。
#[test]
fn returning_to_a_view_shows_its_memory_right_away() {
    let (ui, deck) = deck_window_pumped();
    visit(&ui, &deck, ViewSource::Daily)
        .send(Ok(batch(&["d1"])));
    visit(&ui, &deck, liked()).send(Ok(batch(&["l1"])));

    let _pending = visit(&ui, &deck, ViewSource::Daily);

    assert_eq!(
        shown_ids(&ui),
        ids(&["d1"]),
        "切回推荐,该立刻摆推荐上次那份"
    );
    assert!(
        !ui.global::<Player>().get_tracks_loading(),
        "有缓存就不是加载态"
    );
}

/// A→B→A:第一次进 A 发出的请求在第二次进 A 之后才回来。它已经过期 ——
/// 第二次那份才算数,旧的既不上屏也不写缓存。
#[test]
fn a_superseded_request_for_the_same_view_is_dropped() {
    let (ui, deck) = deck_window_pumped();
    let first = visit(&ui, &deck, liked());
    visit(&ui, &deck, ViewSource::Daily)
        .send(Ok(batch(&["d1"])));
    let second = visit(&ui, &deck, liked());

    first.send(Ok(batch(&["stale"])));
    assert_eq!(
        shown_ids(&ui),
        Vec::<String>::new(),
        "第一次进 A 的旧响应不该上屏"
    );

    second.send(Ok(batch(&["fresh"])));
    assert_eq!(shown_ids(&ui), ids(&["fresh"]));
}

/// 离开之后才回来的响应:写进它自己的缓存,不上屏;再进那个视图时同一步摆出来。
#[test]
fn an_answer_for_a_view_left_behind_fills_only_its_cache() {
    let (ui, deck) = deck_window_pumped();
    let daily = visit(&ui, &deck, ViewSource::Daily);
    visit(&ui, &deck, liked()).send(Ok(batch(&["l1"])));

    daily.send(Ok(batch(&["d1"])));
    assert_eq!(shown_ids(&ui), ids(&["l1"]));

    let _pending = visit(&ui, &deck, ViewSource::Daily);
    assert_eq!(
        shown_ids(&ui),
        ids(&["d1"]),
        "推荐那份已经进了自己的缓存,切回来该立刻摆出"
    );
}

/// 登出之后才回来的响应:作废。不上屏,也不进任何缓存 —— 下一个人登上来
/// 不该看见上一个人的歌单。
#[test]
fn an_answer_arriving_after_logout_is_dropped() {
    let (ui, deck) = deck_window_pumped();
    let account = Rc::new(std::cell::Cell::new(7_u64));
    let reading = account.clone();
    deck.views.set_account(move || reading.get());

    let pending = visit(&ui, &deck, liked());
    account.set(0);
    pending.send(Ok(batch(&["l1"])));

    assert_eq!(
        shown_ids(&ui),
        Vec::<String>::new(),
        "登出后回来的响应不该上屏"
    );

    // 同一个人重新登上来:那份迟到的也没进缓存
    account.set(7);
    let _again = visit(&ui, &deck, liked());
    assert_eq!(
        shown_ids(&ui),
        Vec::<String>::new(),
        "登出后回来的响应不该留在缓存里"
    );
}
