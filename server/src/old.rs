use anyhow::{Context, Result};
use nix::sched::{setns, CloneFlags};
use nix::sys::socket::{
    self, AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr,
};
use std::fs::File;
use std::io::IoSlice;
use std::ops::Bound;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use futures::{StreamExt, SinkExt};
use bytes::Bytes;

/// 核心函数：通过独立的原生线程执行 Namespace 切换和 FD 传递
async fn bootstrap_and_connect(pid: i32, socket_name: String) -> Result<UnixStream> {
    // 使用 oneshot channel 将 FD 从原生线程传回异步环境
    let (tx, rx) = oneshot::channel::<Result<OwnedFd>>();
    // 获得 socketpair
    let (fd_host, fd_agent) = socket::socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::SOCK_CLOEXEC,
    ).context("创建 socketpair 失败")?;
    // 1. 启动一个原生线程（用完即焚，不归还线程池）
    std::thread::spawn(move || {
        // A. 打开目标容器的 NetNS (不再备份 Host NS)
        let target_ns_path: String = format!("/proc/{}/ns/net", pid);
        let target_net_ns = File::open(&target_ns_path).with_context(|| format!("无法打开 PID {} 的网络空间", pid))?;
        // B. 切换到容器内 (仅影响当前这个原生线程)
        setns(&target_net_ns, CloneFlags::CLONE_NEWNET).context("执行 setns 进入容器失败")?;
        // C. 建立引导 Socket (抽象路径)
        let bootstrap_sock = socket::socket(
            AddressFamily::Unix,
            SockType::Datagram,
            SockFlag::SOCK_CLOEXEC,
            None,
        ).context("创建引导套接字失败")?;
        let addr = UnixAddr::new_abstract(socket_name.as_bytes())?;
        socket::bind(bootstrap_sock.as_raw_fd(), &addr).context("绑定抽象套接字失败")?;
        // D. 创建 SocketPair 桥梁
        // E. 接收握手并发送 fd_agent
        let mut buf = [0u8; 1];
        let (_, maybe_addr) = socket::recvfrom::<UnixAddr>(bootstrap_sock.as_raw_fd(), &mut buf).context("等待 Agent 握手超时或失败")?;
        let agent_addr = maybe_addr.context("Agent 地址无效")?;
        let iov = [IoSlice::new(b"\0")];
        let fds = [fd_agent.as_raw_fd()];
        let cmsg = [ControlMessage::ScmRights(&fds)];
        socket::sendmsg::<UnixAddr>(
            bootstrap_sock.as_raw_fd(),
            &iov,
            &cmsg,
            MsgFlags::empty(),
            Some(&agent_addr),
        ).context("SCM_RIGHTS 发送失败")?;
    });
    // 2. 等待原生线程的结果
    let fd_host = rx.await.context("原生线程崩溃或未返回结果")??;
    // 3. 权力交接：将 OwnedFd 转换为 Tokio UnixStream
    // 必须使用 into_raw_fd() 转移所有权，否则 FD 会被 close
    let raw_fd = fd_host.into_raw_fd();
    let std_stream = unsafe { StdUnixStream::from_raw_fd(raw_fd) };
    std_stream.set_nonblocking(true).context("设置非阻塞模式失败")?;
    let tokio_stream = UnixStream::from_std(std_stream).context("转换为异步 UnixStream 失败")?;
    Ok(tokio_stream)
}

#[tokio::main]
async fn main() -> Result<()> {
    // 启动引导程序
    let stream = bootstrap_and_connect(pid, socket).await?;
    // 2. 封装为带长度前缀的帧（4字节头，大端序）
    let codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .new_codec();
    let mut framed = Framed::new(stream, codec);
    // socket 读写器
    // 示例：向 Agent 发送指令并读取回复
    let initial_cmd = "GET_SYSTEM_INFO";
    println!("[Server] 发送指令: {}", initial_cmd);
    framed.send(Bytes::from(initial_cmd)).await?;
    // 4. 循环读写
    while let Some(result) = framed.next().await {
        match result {
            Ok(bytes) => {
                let msg = String::from_utf8_lossy(&bytes);
                println!("[Server] 收到回复: {}", msg);
                // 可以在这里根据逻辑继续写入数据
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                framed.send(Bytes::from("HEARTBEAT_CHECK")).await?;
            }
            Err(e) => {
                eprintln!("[Server] 通讯错误: {}", e);
                break;
            }
        }
    }
    Ok(())
}