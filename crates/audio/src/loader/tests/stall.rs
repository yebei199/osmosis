
use std::io::{Cursor, Read, Seek};
use std::time::Duration;

use crate::codec;

use super::super::*;
use super::{decode_cursor, wav};
use crate::stream_source::buffered;

/// 一次声卡回调的预算。cpal 在 48kHz 立体声上常见的一块缓冲是 1024 帧,
/// 约 21ms —— 回调拖过这个数还没填完,设备就欠载。
pub(super) const CALLBACK_BUDGET: Duration =
    Duration::from_millis(21);

/// 假流停摆多久。取得远大于 [`CALLBACK_BUDGET`],
/// 好让判据不受调度抖动左右。
pub(super) const STALL: Duration =
    Duration::from_millis(300);

/// 卡点的字节位置。必须大于探测格式时的预读量,否则开头那几次读就把卡点
/// 吞了,"卡点之前"这个对照组根本不存在。
pub(super) const STALL_AFTER: u64 = 400_000;

/// 取到多少个采样才算真的跨过了卡点。
///
/// 不能按 `STALL_AFTER ÷ 2 字节` 直接算:解码器是按 32KB 成块读的,
/// 刚好够喂到卡点的那次读**发生在卡点之前**,停摆要到再下一次读才触发。
/// 多留两块的余量,这条测试才不会因为块大小变了就假绿。
pub(super) const PAST_STALL: usize = 300_000;

/// 对照组取多少个采样:约合 10 万字节,远在卡点之内。
pub(super) const BEFORE_STALL: usize = 50_000;

/// 一块回调要多少帧。cpal 在 48kHz 上的常见块大小,正是
/// [`CALLBACK_BUDGET`] 那 21ms 的由来。
pub(super) const CALLBACK_FRAMES: usize = 1024;

/// 跨过卡点要取多少块回调。
///
/// 输入是 44.1kHz 单声道,回调那头是 48kHz 立体声,一个输入采样约合
/// 2.18 个输出采样;[`PAST_STALL`] 个输入采样即约 65 万个输出采样,
/// 除以每块的 1024×2。取整后再多留几块。
pub(super) const BLOCKS_PAST_STALL: usize = 330;

/// 一条读到 [`STALL_AFTER`] 之后就停摆的字节流:模拟 CDN 掉速或连接卡死。
///
/// 卡点之前照常给字节,之后每次 `read` 先睡一觉再给 —— 这正是
/// `StreamDownload` 读到没下完的位置时的行为(它是**阻塞**的)。
///
/// 只让 `read` 停摆,`seek` 照常:解码器探测格式时会来回跳,
/// 那几下不该被算进"网络卡住"里。
struct StallingSource {
    inner: Cursor<Vec<u8>>,
    /// 卡点的字节位置。按流传而不是取全局常量:两组测试要的卡点深浅不同 ——
    /// 一组要它深到能留出对照区,一组要它浅到实时播放一两秒就能撞上。
    stall_after: u64,
    /// 只停摆**一次**。一次抖动就足以证明因果,每次读都睡只是让测试变慢。
    stalled: bool,
}

impl Read for StallingSource {
    fn read(
        &mut self,
        buf: &mut [u8],
    ) -> std::io::Result<usize> {
        if !self.stalled
            && self.inner.position() >= self.stall_after
        {
            self.stalled = true;
            std::thread::sleep(STALL);
        }
        self.inner.read(buf)
    }
}

impl Seek for StallingSource {
    fn seek(
        &mut self,
        pos: std::io::SeekFrom,
    ) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// 取 `count` 个采样,返回**单次**取样最久的那一次花了多长。
///
/// 看单次而不是总时长:声卡回调是一次一次被叫醒的,拖垮它的是某一次
/// 交不出数据,平均值反而会把这件事摊平到看不见。
fn longest_pull(count: usize) -> Duration {
    // 10 秒的 WAV,足够让卡点落在中间而不是开头。
    let source = StallingSource {
        inner: Cursor::new(wav(441_000)),
        stall_after: STALL_AFTER,
        stalled: false,
    };
    let mut decoder =
        decode(source, None).expect("合法 WAV 应能解码");

    let mut worst = Duration::ZERO;
    for _ in 0..count {
        let started = std::time::Instant::now();
        if decoder.next().is_none() {
            break;
        }
        worst = worst.max(started.elapsed());
    }
    worst
}

/// **字节流一卡,取样就跟着卡。**
///
/// 生产环境里调 `next()` 的是 cpal 的声卡回调(rodio-0.22 `stream.rs:527`
/// 那句 `samples.next()`),所以这里量到的阻塞时长,就是回调交不出数据的
/// 时长。这条钉死的是欠载的**成因**,不是欠载本身 —— 后者要真声卡。
#[test]
fn a_stalled_byte_stream_blocks_the_sample_pull() {
    let worst = longest_pull(PAST_STALL);

    assert!(
        worst >= STALL,
        "跨过卡点的那次取样只花了 {worst:?},没被流阻塞住"
    );
}

/// 阻塞多久算出事,得有个数,免得日后靠感觉争论。
///
/// 与上一条量的是同一件事,判据不同:上一条问"是不是被阻塞了",
/// 这一条问"阻塞得够不够让声卡欠载"。前者是机制,后者是后果。
#[test]
fn the_stall_outlasts_one_callback_budget() {
    let worst = longest_pull(PAST_STALL);

    assert!(
        worst > CALLBACK_BUDGET,
        "最久的一次取样 {worst:?} 没超过回调预算 {CALLBACK_BUDGET:?},欠载的推论不成立"
    );
}

/// 对照组:卡点之前的取样必须很快回来。
///
/// 没有这条,上面两条也可能只是"这条流从头到尾都慢"造成的,
/// 证不出"卡在特定位置"与"取样阻塞"之间的因果。
#[test]
fn samples_before_the_stall_point_come_back_promptly() {
    let worst = longest_pull(BEFORE_STALL);

    assert!(
        worst < CALLBACK_BUDGET,
        "卡点之前就已经有一次取样花了 {worst:?},这条流本身就慢,对照组不成立"
    );
}

/// 按生产的组装方式把一路源接到 Mixer 上,交回声卡回调那一端。
///
/// 与 [`emit`](../../ui/src/music.rs) + [`Player::play`] 同形:
/// normalize → Tee → `rodio::Player` → `Mixer`。唯一缺的是 OS 设备,
/// 而它的职责恰好由调用方扮演 —— 按块来取,取不到就欠载。
///
/// `Player` 必须一起交回去:drop 掉它会把播放停掉(rodio `player.rs:345`),
/// 留在函数里的话回调那头立刻就只剩静音。
fn callback_end<S>(
    source: S,
) -> (rodio::Player, rodio::mixer::MixerSource)
where
    S: rodio::Source + Send + 'static,
{
    let (mixer, out) = rodio::mixer::mixer(
        rodio::ChannelCount::new(codec::SYNC_CHANNELS)
            .expect("声道数是编译期常量,非零"),
        rodio::SampleRate::new(codec::SYNC_SAMPLE_RATE)
            .expect("采样率是编译期常量,非零"),
    );
    let player = rodio::Player::connect_new(&mixer);

    let (tee, _branch) = codec::Tee::new(
        codec::normalize(source),
        codec::BRANCH_CAPACITY,
    );
    player.append(tee);
    player.play();

    (player, out)
}

/// 扮演 cpal 的回调:一块一块地取,返回**单块**取得最久的那一次。
///
/// `stalling` 决定喂的是会停摆的流还是老实流 —— 两条测试的差别只有这一个,
/// 别的都得一模一样,否则对照组证不了东西。
fn longest_callback_block(stalling: bool) -> Duration {
    let bytes = wav(441_000);
    let source = StallingSource {
        inner: Cursor::new(bytes),
        stall_after: STALL_AFTER,
        // 老实流:一上来就当作"已经停摆过了",于是永不睡。
        stalled: !stalling,
    };
    let decoder =
        decode(source, None).expect("合法 WAV 应能解码");
    let (_player, mut out) = callback_end(decoder);

    let mut worst = Duration::ZERO;
    for _ in 0..BLOCKS_PAST_STALL {
        let started = std::time::Instant::now();
        // Mixer 没源可放时给静音而不是结束,跟真设备一样,所以不必判 None。
        for _ in 0..CALLBACK_FRAMES
            * codec::SYNC_CHANNELS as usize
        {
            out.next();
        }
        worst = worst.max(started.elapsed());
    }
    worst
}

/// 一次驱动的结果:最慢的一块花了多久,以及这一路取到的非静音采样数。
///
/// 两个数缺一不可。只看时长的话,一个**什么都不放**的实现也能满分 ——
/// 静音取得飞快。非静音计数是防止这条测试变成空断言的那一半。
struct Driven {
    worst: Duration,
    audible: usize,
}

/// 扮演 cpal 的回调,并且**按实时节奏**取:每块之间补足到 21ms 再取下一块。
///
/// [`longest_callback_block`] 那种不停地取,消费端等于无限快,任何缓冲都
/// 来不及填 —— 拿它测缓冲只会得到"缓冲没用"的假结论。真声卡是按节奏来的,
/// 缓冲能起作用正是因为这个节奏留出了填的时间。
fn drive_paced<S>(source: S, blocks: usize) -> Driven
where
    S: rodio::Source + Send + 'static,
{
    let (_player, mut out) = callback_end(source);

    let mut worst = Duration::ZERO;
    let mut audible = 0;
    for _ in 0..blocks {
        let started = std::time::Instant::now();
        for _ in 0..CALLBACK_FRAMES
            * codec::SYNC_CHANNELS as usize
        {
            if out.next().is_some_and(|s| s != 0.0) {
                audible += 1;
            }
        }
        let spent = started.elapsed();
        worst = worst.max(spent);
        // 补足这一块的实时时长。真设备就是这样:填完就等下一次被叫醒。
        if let Some(rest) =
            CALLBACK_BUDGET.checked_sub(spent)
        {
            std::thread::sleep(rest);
        }
    }

    Driven { worst, audible }
}

/// 按实时节奏播时的卡点。比 [`STALL_AFTER`] 浅得多:44.1kHz 单声道下
/// 15 万字节约合第 1.7 秒,**不带缓冲**那一路也能在两秒内老老实实播到。
///
/// 深浅在这里是判据的一部分:卡点若深到实时播不到,不带缓冲的那一路会
/// 因为"根本没撞上"而通过,测试就成了空的(第一版正是这么假绿的)。
pub(super) const PACED_STALL_AFTER: u64 = 150_000;

/// 按实时节奏跑多少块。100 块约合 2.1 秒,足够越过
/// [`PACED_STALL_AFTER`],又不至于让测试变成秒级的枯等。
pub(super) const PACED_BLOCKS: usize = 100;

/// 一条会在 [`PACED_STALL_AFTER`] 处停摆一次的解码器。
fn stalling_decoder() -> rodio::Decoder<StallingSource> {
    decode_stalling(StallingSource {
        inner: Cursor::new(wav(441_000)),
        stall_after: PACED_STALL_AFTER,
        stalled: false,
    })
    .expect("合法 WAV 应能解码")
}

/// **阻塞会一路传到声卡回调,中间没有任何一层挡得住。**
///
/// 补的是上面三条的空档:它们只到解码器为止,而生产里解码器与设备之间还
/// 隔着 `Player`、队列、`Mixer`。这三层里但凡有一层自带缓冲,阻塞就传不
/// 过去,"网络卡一下就欠载"的推论也就不成立 —— 那是从源码读出来的结论,
/// 得有东西钉住它。
#[test]
fn a_stalled_stream_starves_the_simulated_callback() {
    let worst = longest_callback_block(true);

    assert!(
        worst > CALLBACK_BUDGET,
        "最久的一块只花了 {worst:?},没超过回调预算 {CALLBACK_BUDGET:?} —— 阻塞没传到回调这头"
    );
}

/// 对照组:流不卡时,每一块都在预算内取完。
///
/// 排除"这条链路本身就跟不上实时"这个替代解释 —— 尤其是 normalize 那步的
/// 44.1k→48k 重采样,它是链上唯一有点算力开销的环节。
#[test]
fn an_unstalled_stream_keeps_the_simulated_callback_on_time()
 {
    let worst = longest_callback_block(false);

    assert!(
        worst < CALLBACK_BUDGET,
        "流没卡,却有一块花了 {worst:?}(预算 {CALLBACK_BUDGET:?}) —— 慢的是链路本身,不是网络"
    );
}

/// **解码搬出回调之后,流卡住不再拖慢回调。**
///
/// 这是把解码挪到自己线程上的验收判据,两个断言各挡一半:时长挡"回调被
/// 拖住",非静音计数挡"什么都不放也算过" —— 静音取得飞快,只看时长的话
/// 一个把声音全丢掉的实现能拿满分。
#[test]
fn buffering_carries_the_callback_through_a_stall() {
    let driven = drive_paced(
        buffered(codec::normalize(stalling_decoder())),
        PACED_BLOCKS,
    );

    assert!(
        driven.worst < CALLBACK_BUDGET,
        "有一块花了 {:?},超过回调预算 {CALLBACK_BUDGET:?} —— 停摆仍然穿到了回调这头",
        driven.worst
    );
    assert!(
        driven.audible
            > PACED_BLOCKS
                * CALLBACK_FRAMES
                * codec::SYNC_CHANNELS as usize
                / 2,
        "只取到 {} 个非静音采样,缓冲扛住了卡顿却没把声音送出来",
        driven.audible
    );
}

/// **缓冲不能把"放完了"吃掉。**
///
/// 自动续播靠 `Player::empty()` 知道该切下一首,而它为真的前提是源返回
/// `None`。缓冲线程跑完要丢掉发送端,通道排空后 [`ChannelSource`] 才会结束 ——
/// 漏了这一步,每首歌放完都会停在原地,再也不切下一首。
///
/// 真正的判据是**这个测试能跑完**:结束不了的话它会挂死,而不是断言失败。
#[test]
fn a_buffered_song_still_ends() {
    // 100ms 的音,归一到 48kHz 立体声约合 9600 个采样。
    let decoder = decode_cursor(wav(4_410))
        .expect("合法 WAV 应能解码");

    let played =
        buffered(codec::normalize(decoder)).count();

    assert!(
        played >= 9_600,
        "只放出 {played} 个采样,短了一截 —— 缓冲把尾巴吃掉了"
    );
}

/// 解一条长度未知的流。真机上上游不给 `Content-Length` 时就是这样,
/// 那时这一首只能往前跳。
fn decode_stalling(
    source: StallingSource,
) -> Result<rodio::Decoder<StallingSource>, AudioError> {
    decode(source, None)
}
