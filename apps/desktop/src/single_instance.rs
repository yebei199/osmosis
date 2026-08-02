//! 启动锁:同一时刻只允许一个桌面实例。
//!
//! 多开的代价不是"多一个窗口":两个实例会抢同一块声卡、各放各的歌,而 MCP 那个
//! 固定端口只有先起来的那个抢得到 —— 于是调试时连上的可能是上一次忘了关的实例,
//! 看到的界面根本不是刚改的那份。`just desktop-dev` 的 `mcp-port-free` 只守
//! MCP 端口,而且只在那条配方里;裸 `cargo run` 一路畅通。
//!
//! 实现用 Linux 的**抽象命名空间** unix socket,不用锁文件:抽象地址随进程消失,
//! 没有"上次崩了留下一个锁,从此再也起不来"这种问题,也不必挑一个目录。
//!
//! 别的操作系统上没有抽象命名空间,那里暂时不设锁 —— 现在只有 linux 这一端在跑。

use std::io;

/// 锁的名字。抽象地址不占文件系统,但仍然是全局的,所以取一个不会撞的名字。
#[cfg(target_os = "linux")]
const LOCK_NAME: &str = "osmosis-desktop.lock";

/// 拿到的锁。**活多久,锁多久** —— 丢掉它就等于开门,所以调用方要把它一直留着。
///
/// 进程被 kill 也会释放:内核关掉 socket,抽象地址随之消失。
#[must_use = "丢掉它锁就没了,得留到进程结束"]
pub struct InstanceLock {
    #[cfg(target_os = "linux")]
    _socket: std::os::unix::net::UnixListener,
}

/// 占住这台机器上的"桌面实例"这个位置。已经有人占着就返回 `Err`。
#[cfg(target_os = "linux")]
pub fn claim() -> io::Result<InstanceLock> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    let address =
        SocketAddr::from_abstract_name(LOCK_NAME)?;
    // bind 失败(AddrInUse)就是"已经有一个在跑"。不去连它、不去问它是谁 ——
    // 这里只回答"能不能起",唤醒已有窗口是另一件事(眼下没有那个需求)。
    let socket = UnixListener::bind_addr(&address)?;

    Ok(InstanceLock { _socket: socket })
}

/// 非 Linux 上不设锁:抽象命名空间是 Linux 专有的,而现在只有 linux 这一端在跑。
/// 真要支持时,那里该用带 flock 的锁文件,不是这一套。
#[cfg(not(target_os = "linux"))]
pub fn claim() -> io::Result<InstanceLock> {
    Ok(InstanceLock {})
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// **第二把锁必须拿不到。**
    ///
    /// 这条同时钉住了另一半:第一把还活着的时候才算数。锁要是随手就被释放
    /// (比如 `claim` 里没把 socket 留住),第二次照样能成,这个门就是假的。
    #[test]
    fn a_second_instance_cannot_claim_the_lock() {
        let Ok(first) = claim() else {
            // 同一台机器上真的有实例在跑时跳过 —— 那时这条测的是别人的锁。
            return;
        };

        assert!(
            claim().is_err(),
            "第一把锁还握着,第二把不该拿得到"
        );

        // 放开之后要能再拿到:锁是"活多久锁多久",不是一次性的。
        drop(first);
        assert!(
            claim().is_ok(),
            "上一个实例退了,新的该起得来"
        );
    }
}
