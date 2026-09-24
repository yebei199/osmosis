//! synctest:两端各自出声、共享时间轴的最小原型(#137 ②)。用法见 `../README.md`。

mod clock;
mod media;
mod player;
mod timeline;
mod wav;

#[cfg(target_os = "android")]
mod aaudio;
#[cfg(target_os = "linux")]
mod out_linux;

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clock::{Estimator, Msg, mono_ns};
use player::{LocalPlan, Shared};
use timeline::{PlanDto, Segment};

const USAGE: &str = "\
synctest media  --out <wav> [--secs 900]
synctest serve  --media <wav> --channel <0|1> [--port 7010] [--start-in 8] [--secs 300]
                [--seek-at <秒> --seek-by <秒>] [--dev pc] [--gain 1.0]
synctest play   --server <ip:port> --media <wav> --channel <0|1> [--dev phone] [--gain 1.0]
synctest record --out <wav> --secs <秒>            (仅安卓)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("media") => cmd_media(&args),
        Some("serve") => cmd_serve(&args),
        Some("play") => cmd_play(&args),
        Some("record") => cmd_record(&args),
        _ => Err(USAGE.to_owned()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn num<T: std::str::FromStr>(args: &[String], name: &str, default: Option<T>) -> Result<T, String> {
    match opt(args, name) {
        Some(v) => v.parse().map_err(|_| format!("{name} 不是数: {v}")),
        None => default.ok_or_else(|| format!("缺 {name}\n{USAGE}")),
    }
}

fn cmd_media(args: &[String]) -> Result<(), String> {
    let out = opt(args, "--out").ok_or(USAGE)?;
    let secs: u32 = num(args, "--secs", Some(900))?;
    let samples = media::generate(secs);
    wav::write(out, 2, media::RATE, &samples).map_err(|e| e.to_string())?;
    println!("{out}: {secs}s, identity {:016x}", media::identity(&samples));
    Ok(())
}

/// 读媒体、核身份、取出这一端要放的声道。
fn load_media(args: &[String]) -> Result<(Arc<Vec<f32>>, u32, u64), String> {
    let path = opt(args, "--media").ok_or(USAGE)?;
    let channel: usize = num(args, "--channel", None)?;
    let (channels, rate, samples) = wav::read(path).map_err(|e| format!("{path}: {e}"))?;
    if channel >= usize::from(channels) {
        return Err(format!("{path} 只有 {channels} 个声道"));
    }
    let id = media::identity(&samples);
    Ok((Arc::new(media::channel(&samples, usize::from(channels), channel)), rate, id))
}

fn cmd_serve(args: &[String]) -> Result<(), String> {
    let (media, rate, id) = load_media(args)?;
    let port: u16 = num(args, "--port", Some(7010))?;
    let start_in: f64 = num(args, "--start-in", Some(8.0))?;
    let secs: f64 = num(args, "--secs", Some(300.0))?;
    let dev = opt(args, "--dev").unwrap_or("pc").to_owned();
    let gain: f32 = num(args, "--gain", Some(1.0))?;

    let start = mono_ns() + (start_in * 1e9) as i64;
    let mut segments = vec![Segment { at_ns: start, pos: 0.0 }];
    if let (Some(at), Some(by)) = (opt(args, "--seek-at"), opt(args, "--seek-by")) {
        let at: f64 = at.parse().map_err(|_| "--seek-at 不是数")?;
        let by: f64 = by.parse().map_err(|_| "--seek-by 不是数")?;
        segments.push(Segment {
            at_ns: start + (at * 1e9) as i64,
            pos: (at + by) * f64::from(rate),
        });
    }
    let plan = PlanDto {
        version: 1,
        media_hash: id,
        rate,
        segments: segments.clone(),
        end_ns: start + (secs * 1e9) as i64,
    };

    let sock = UdpSocket::bind(("0.0.0.0", port)).map_err(|e| e.to_string())?;
    let served = plan.clone();
    std::thread::spawn(move || {
        loop {
            let mut buf = [0u8; 2048];
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            let t1 = mono_ns();
            match serde_json::from_slice::<Msg>(&buf[..n]) {
                Ok(Msg::Ping { t0 }) => {
                    clock::send(&sock, from, &Msg::Pong { t0, t1, t2: mono_ns() })
                }
                Ok(Msg::GetPlan) => clock::send(&sock, from, &Msg::Plan(served.clone())),
                _ => {}
            }
        }
    });

    let shared = Shared::new();
    *shared.plan.lock().unwrap() = Some(LocalPlan {
        segments,
        end_ns: plan.end_ns,
    });
    let output = start_output(media.clone(), rate, shared.clone(), gain)?;
    eprintln!(
        "serve: 端口 {port},媒体 {id:016x},{start_in}s 后起播,放 {secs}s,输出 {}",
        describe(&output)
    );
    // 服务端这台也会换路由(连上/断开蓝牙),同样要重开输出流。
    status_loop(&dev, &shared, start, plan.end_ns, output, Some((media, rate, gain)), |_| (0.0, 0.0));
    Ok(())
}

fn cmd_play(args: &[String]) -> Result<(), String> {
    let (media, rate, id) = load_media(args)?;
    let server: SocketAddr = opt(args, "--server")
        .ok_or(USAGE)?
        .parse()
        .map_err(|e| format!("--server: {e}"))?;
    let dev = opt(args, "--dev").unwrap_or("phone").to_owned();
    let gain: f32 = num(args, "--gain", Some(1.0))?;
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;

    let mut est = Estimator::default();
    for _ in 0..5 {
        if let Some(s) = clock::burst(&sock, server, 8) {
            est.add(s);
        }
    }
    let rtt = est.min_rtt().ok_or("连不上服务端(没有一次往返回来)")?;
    let plan = fetch_plan(&sock, server)?;
    if plan.media_hash != id {
        return Err(format!(
            "媒体对不上:服务端 {:016x},本机 {id:016x} —— 不放",
            plan.media_hash
        ));
    }
    eprintln!("play: 服务端 {server},最小 RTT {:.2}ms,媒体 {id:016x}", rtt as f64 / 1e6);

    let shared = Shared::new();
    let localize = |est: &Estimator| -> Option<LocalPlan> {
        let segments = plan
            .segments
            .iter()
            .map(|s| Some(Segment { at_ns: est.to_local(s.at_ns)?, pos: s.pos }))
            .collect::<Option<Vec<_>>>()?;
        Some(LocalPlan {
            segments,
            end_ns: est.to_local(plan.end_ns)?,
        })
    };
    let first = localize(&est).ok_or("换算计划失败")?;
    let (start, end) = (first.segments[0].at_ns, first.end_ns);
    *shared.plan.lock().unwrap() = Some(first);
    let output = start_output(media.clone(), rate, shared.clone(), gain)?;
    eprintln!("play: 输出 {}", describe(&output));

    let mut est = est;
    let resync = move |shared: &Arc<Shared>| {
        if let Some(s) = clock::burst(&sock, server, 8) {
            est.add(s);
        }
        if let Some(p) = localize(&est) {
            *shared.plan.lock().unwrap() = Some(p);
        }
        let now = mono_ns();
        (
            est.offset_at(now).unwrap_or(0.0),
            est.min_rtt().unwrap_or(0) as f64,
        )
    };
    status_loop(&dev, &shared, start, end, output, Some((media, rate, gain)), resync);
    Ok(())
}

fn fetch_plan(sock: &UdpSocket, server: SocketAddr) -> Result<PlanDto, String> {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
    for _ in 0..20 {
        clock::send(sock, server, &Msg::GetPlan);
        for _ in 0..4 {
            if let Some((Msg::Plan(plan), _)) = clock::recv(sock) {
                return Ok(plan);
            }
        }
    }
    Err("拿不到计划".to_owned())
}

/// 每秒一行 JSON 状态,直到计划结束两秒后。`tick` 是客户端的重新校时,返回 (偏移, 最小 RTT)。
/// `reopen_with` 给了就在输出流断开时关掉重开(安卓的路由切换)。
fn status_loop(
    dev: &str,
    shared: &Arc<Shared>,
    start: i64,
    end: i64,
    output: OutputHandle,
    reopen_with: Option<(Arc<Vec<f32>>, u32, f32)>,
    mut tick: impl FnMut(&Arc<Shared>) -> (f64, f64),
) {
    let mut output = Some(output);
    let mut reopens = 0u32;
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        let (offset, rtt) = tick(shared);
        let now = mono_ns();
        if let Some((media, rate, gain)) = &reopen_with
            && output.as_ref().is_some_and(disconnected)
        {
            // 重开后第一块会因误差过大直接跳回计划位置。
            let t0 = mono_ns();
            output = None;
            match start_output(media.clone(), *rate, shared.clone(), *gain) {
                Ok(o) => {
                    reopens += 1;
                    eprintln!(
                        "{dev}: 输出流重开({:.0}ms),{}",
                        (mono_ns() - t0) as f64 / 1e6,
                        describe(&o)
                    );
                    output = Some(o);
                }
                Err(e) => eprintln!("{dev}: 重开失败: {e}"),
            }
        }
        let s = &shared.stats;
        let line = serde_json::json!({
            "dev": dev,
            "t": (now - start) as f64 / 1e9,
            "err_ms": s.err_ns.load(Ordering::Relaxed) as f64 / 1e6,
            "corr_ppm": s.corr_ppm.load(Ordering::Relaxed),
            "jumps": s.jumps.load(Ordering::Relaxed),
            "lat_ms": s.latency_ns.load(Ordering::Relaxed) as f64 / 1e6,
            "ts_ok": s.ts_ok.load(Ordering::Relaxed),
            "ts_est": s.ts_estimated.load(Ordering::Relaxed),
            "xruns": output.as_ref().map_or(-1, xruns),
            "reopens": reopens,
            "off_ms": offset / 1e6,
            "rtt_ms": rtt / 1e6,
            "playing": s.playing.load(Ordering::Relaxed),
        });
        println!("{line}");
        if now > end + 2_000_000_000 {
            break;
        }
    }
}

// ---- 两个后端的薄封装:让上面的循环不关心在哪个平台上 ----

#[cfg(target_os = "linux")]
type OutputHandle = out_linux::Output;
#[cfg(target_os = "android")]
type OutputHandle = aaudio::Output;

fn start_output(
    media: Arc<Vec<f32>>,
    rate: u32,
    shared: Arc<Shared>,
    gain: f32,
) -> Result<OutputHandle, String> {
    #[cfg(target_os = "linux")]
    return out_linux::start(media, rate, shared, gain);
    #[cfg(target_os = "android")]
    return aaudio::start(media, rate, shared, gain);
}

#[cfg(target_os = "linux")]
fn describe(o: &OutputHandle) -> String {
    format!("cpal {} @{}Hz", o.device, o.rate)
}
#[cfg(target_os = "android")]
fn describe(o: &OutputHandle) -> String {
    format!("AAudio 设备 {} @{}Hz", o.device_id(), o.rate)
}

/// 电脑侧的 cpal 流不会自己断;安卓侧路由一切换流就断,要重开。
#[cfg(target_os = "linux")]
fn disconnected(_: &OutputHandle) -> bool {
    false
}
#[cfg(target_os = "android")]
fn disconnected(o: &OutputHandle) -> bool {
    o.disconnected.load(Ordering::Relaxed)
}

#[cfg(target_os = "linux")]
fn xruns(_: &OutputHandle) -> i64 {
    -1
}
#[cfg(target_os = "android")]
fn xruns(o: &OutputHandle) -> i64 {
    i64::from(o.xruns())
}

fn cmd_record(args: &[String]) -> Result<(), String> {
    let out = opt(args, "--out").ok_or(USAGE)?;
    let secs: u32 = num(args, "--secs", None)?;
    #[cfg(target_os = "android")]
    return aaudio::record(out, secs);
    #[cfg(not(target_os = "android"))]
    {
        let _ = (out, secs);
        Err("record 只在安卓上有".to_owned())
    }
}
