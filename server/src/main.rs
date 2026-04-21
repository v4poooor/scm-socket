use nix::sys::socket::{
    self, AddressFamily, SockFlag, SockType, UnixAddr, sendmsg, ControlMessage, MsgFlags
};
use procfs::process::all_processes;
use std::collections::HashSet;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::Command;
use std::sync::Arc;
use std::fs;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::io::Interest;
use tokio::time::{self, Duration};
use anyhow::{Context, Result};
use std::os::unix::io::{FromRawFd, IntoRawFd};
use nix::sched::{setns, CloneFlags};
use std::fs::File;
use bytes::Bytes;
use tokio_util::codec::LengthDelimitedCodec;
use tokio_util::codec::Framed;
use futures::{StreamExt, SinkExt};

#[tokio::main]
async fn main() -> Result<()> {
    // 1. 创建 socketpair: fd_socket (本地留用) 和 fd_agent (准备发给 Java)
    let (fd_socket, fd_agent) = socket::socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::SOCK_NONBLOCK,
    )?;
    
    // 将 fd_socket 转换为 tokio 可用的异步读写对象
    let fd_socket_std = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd_socket.into_raw_fd()) };
    fd_socket_std.set_nonblocking(true)?;
    let async_fd_socket = tokio::net::UnixStream::from_std(fd_socket_std).context("转换为异步 UnixStream 失败")?;

    println!("Socketpair 创建成功。fd_agent 为: {:?}", fd_agent);
    // 记录已经注入过的 PID
    let injected_pids = Arc::new(Mutex::new(HashSet::new()));
    let injected_pids_clone = Arc::clone(&injected_pids);

    // 2. 启动线程 A：每 30s 遍历 /proc
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            println!("正在扫描 Java 进程...");
            if let Ok(procs) = all_processes() {
                for p in procs {
                    if let Ok(proc) = p {
                        if is_java_process(&proc) {
                            let pid = proc.pid();
                            let mut pids = injected_pids_clone.lock().await;
                            if !pids.contains(&pid) {
                                println!("发现新 Java 进程 PID: {}, 准备注入...", pid);
                                // 3. 启动注入流程
                                if let Err(e) = run_injection_flow(pid, fd_agent.as_raw_fd()).await {
                                    eprintln!("PID {} 注入失败: {}", pid, e);
                                } else {
                                    pids.insert(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    // 4. 监听 fd_socket
    let codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .new_codec();
    let mut framed = Framed::new(async_fd_socket, codec);
    let initial_cmd = "GET_SYSTEM_INFO";
    println!("[Server] 发送指令: {}", initial_cmd);
    framed.send(Bytes::from(initial_cmd)).await?;
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

/// 判断是否为 Java 进程
fn is_java_process(proc: &procfs::process::Process) -> bool {
    if let Ok(stat) = proc.stat() {
        if stat.comm == "java" { return true; }
    }
    if let Ok(cmd) = proc.cmdline() {
        return cmd.iter().any(|arg| arg.contains("java"));
    }
    false
}

fn switch_namespace(pid: i32) -> Result<()> {
    let fd = File::open(format!("/proc/{}/ns/net", pid))?;
    setns(fd, CloneFlags::CLONE_NEWNET)?;
    Ok(())
}

/// 注入流程核心逻辑
async fn run_injection_flow(pid: i32, fd_to_send: RawFd) -> Result<()> {
    let root_path = format!("/proc/{}/root/Server/", pid);
    let _ = fs::create_dir_all(&root_path).context(format!("{} file create failed.", root_path))?;

    let jattach = String::from("/Server/plugin/rasp/lib/java/jattach");
    let agent = String::from("/mnt/hgfs/scm-rights/scm-agent/target/Server-agent-1.0-SNAPSHOT.jar");
    let container_Server_path = format!("/proc/{}/root/Server/Server-agent.jar", pid);

    let _ = fs::copy(&agent, &container_Server_path).context(format!("{} copy failed.", agent))?;

    println!("切换到目标 net 命名空间。");
    switch_namespace(pid).context("命名空间切换失败.")?;
    // 启动 \0rasptempsock 抽象套接字监听
    // "\0" 在 Rust 中表示抽象命名空间
    let abstract_path = "\0rasptempsock";
    let shared_fd_agent = Arc::new(fd_to_send);
    let listener = UnixListener::bind(abstract_path)?;
    tokio::spawn(async move {
        loop {
            // 持续 accept，直到程序退出
            match listener.accept().await {
                Ok((stream, _)) => {
                    println!("接收到连接信号，准备通过 scm_rights 发送文件 fd");
                    let fd_to_send = shared_fd_agent.as_raw_fd();
                    // 为每个连进来的 Agent 开启一个独立的处理任务
                    let _ = stream.writable().await;
                    // 发送 FD
                    let res = stream.try_io(Interest::WRITABLE, || {
                        let iov = [std::io::IoSlice::new(b"handshake")];
                        let cmsg = [ControlMessage::ScmRights(&[fd_to_send])];
                        println!("发送");
                        sendmsg::<UnixAddr>(
                            stream.as_raw_fd(),
                            &iov,
                            &cmsg,
                            MsgFlags::empty(),
                            None,
                        ).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                    });
                    match res {
                        Ok(_) => println!("发送文件 fd 成功"),
                        Err(e) => eprintln!("发送文件 fd 失败，错误：{:?}", e),
                    }
                    break;
                }
                Err(e) => {
                    eprintln!("Accept 循环异常: {}", e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    // 启动 jattach
    let pid_str = pid.to_string();
    let _ = Command::new(jattach)
            .args(&[&pid_str, "load", "instrument", "false", "/Server/Server-agent.jar"])
            .status();

    Ok(())
}