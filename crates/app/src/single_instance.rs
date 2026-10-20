//! 同一用户的图形会话只运行一个实例，重复启动仅请求唤出已有窗口。
//!
//! 唯一性由操作系统文件锁保证，而非端口占用或 PID 猜测。锁与 IPC 元数据分开，
//! Windows 的排他字节锁不会阻止副实例读取端口。崩溃后的旧元数据没有锁定效力。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(180);
const RETRY_DELAY: Duration = Duration::from_millis(25);
const MAX_METADATA_BYTES: u64 = 512;
const MAX_REQUEST_BYTES: usize = 128;

#[derive(Debug)]
pub enum Launch {
    Primary(InstanceGuard),
    Activated,
}

/// 必须由 main 持有直到 GUI 事件循环退出；不要只保留 receiver 后提前丢弃本对象。
#[derive(Debug)]
pub struct InstanceGuard {
    _lock: File,
    receiver: Option<Receiver<()>>,
    stop: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl InstanceGuard {
    pub fn take_activation_receiver(&mut self) -> Option<Receiver<()>> {
        self.receiver.take()
    }

    pub fn activation_accepting(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.accepting)
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            // 每个客户端都有总超时，恶意或异常连接不能让退出无限等待。
            let _ = server.join();
        }
        // 文件最后自动关闭并释放锁。不 unlink 锁文件，避免其他进程锁到不同 inode。
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    version: u8,
    port: u16,
    primary_pid: u32,
    token: String,
}

impl Endpoint {
    fn valid(&self) -> bool {
        self.version == 1
            && self.port != 0
            && self.primary_pid != 0
            && self.token.len() == 64
            && self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

/// 显式调试与后台模式保留多实例能力；隔离预览由 main 更早返回，不触碰用户实例目录。
pub(crate) fn protects_gui(args: &crate::cli::Args) -> bool {
    !args.no_tray
        && !args.loopback
        && !args.selftest
        && args.record.is_none()
        && args.replay.is_none()
        && (args.tray || (!args.serve && args.connect.is_none()))
}

pub fn acquire_or_activate() -> io::Result<Launch> {
    let directory = crate::config::config_path()
        .and_then(|path| path.parent().map(|path| path.join("instance")))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "无法确定当前用户的应用目录，未启动额外实例。",
            )
        })?;
    acquire_at(&directory, ACTIVATION_TIMEOUT)
}

fn acquire_at(directory: &Path, timeout: Duration) -> io::Result<Launch> {
    prepare_directory(directory)?;
    let file = open_private(&directory.join("instance.lock"), false)?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return start_primary(file, directory),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => return Err(error),
        }
        // 首个实例可能尚未写完元数据；重读并重试，但不能把激活失败当成获锁成功。
        if let Ok(endpoint) = read_endpoint(directory)
            && notify_existing(&endpoint, deadline).is_ok()
        {
            return Ok(Launch::Activated);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Fesilent Mouse 已在运行，但暂未响应窗口唤出请求。请从系统托盘打开；未启动第二个实例。",
            ));
        };
        std::thread::sleep(RETRY_DELAY.min(remaining));
    }
}

fn prepare_directory(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    if fs::symlink_metadata(directory)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "实例目录不能是符号链接。",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    // Windows 目录位于当前用户 AppData，文件沿用其用户配置目录的 ACL。
    Ok(())
}

fn open_private(path: &Path, truncate: bool) -> io::Result<File> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "实例记录不能是符号链接。",
        ));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn start_primary(file: File, directory: &Path) -> io::Result<Launch> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    // 使用现有密码学随机源生成一次性秘密。只在内存创建，不读取或保存设备身份。
    let random = ms_net::cert::Identity::generate()
        .map_err(|_| io::Error::other("无法创建本机窗口唤出凭据。"))?;
    let token: String = random
        .fingerprint()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let endpoint = Endpoint {
        version: 1,
        port: listener.local_addr()?.port(),
        primary_pid: std::process::id(),
        token: token.clone(),
    };
    let data = serde_json::to_vec(&endpoint).map_err(io::Error::other)?;
    let mut metadata = open_private(&directory.join("instance.json"), true)?;
    metadata.write_all(&data)?;
    metadata.sync_all()?;
    drop(metadata);

    // 合并连点：GUI 尚未初始化或已有唤出请求时最多排队一项，避免无界积压。
    let (sender, receiver) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let accepting = Arc::new(AtomicBool::new(true));
    let server_accepting = Arc::clone(&accepting);
    let server_stop = Arc::clone(&stop);
    let server = std::thread::Builder::new()
        .name("fesilent-instance".into())
        .spawn(move || {
            while !server_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, address)) if address.ip().is_loopback() => {
                        handle_request(stream, &token, &sender, &server_accepting);
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })?;
    Ok(Launch::Primary(InstanceGuard {
        _lock: file,
        receiver: Some(receiver),
        stop,
        accepting,
        server: Some(server),
    }))
}

fn read_endpoint(directory: &Path) -> io::Result<Endpoint> {
    let path = directory.join("instance.json");
    if fs::symlink_metadata(&path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "实例记录无效。",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "实例记录过大。"));
    }
    let endpoint: Endpoint = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if !endpoint.valid() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "实例记录无效。"));
    }
    Ok(endpoint)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "窗口唤出请求超时。"))
}

fn read_frame(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(MAX_REQUEST_BYTES);
    while frame.len() < MAX_REQUEST_BYTES {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let mut chunk = [0_u8; MAX_REQUEST_BYTES];
        let capacity = MAX_REQUEST_BYTES - frame.len();
        let count = stream.read(&mut chunk[..capacity])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "窗口唤出连接提前关闭。",
            ));
        }
        frame.extend_from_slice(&chunk[..count]);
        if let Some(newline) = frame.iter().position(|byte| *byte == b'\n') {
            if newline + 1 != frame.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "窗口唤出请求格式无效。",
                ));
            }
            return Ok(frame);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "窗口唤出请求过长。",
    ))
}

fn notify_existing(endpoint: &Endpoint, deadline: Instant) -> io::Result<()> {
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, endpoint.port));
    let connect_timeout = REQUEST_TIMEOUT.min(remaining(deadline)?);
    let mut stream = TcpStream::connect_timeout(&address, connect_timeout)?;
    // 只有从本用户私有目录读到有效端点后才授权前台；失败不影响单实例约束。
    #[cfg(target_os = "windows")]
    let _ = ms_platform_windows::window::allow_existing_instance_foreground(endpoint.primary_pid);
    stream.set_write_timeout(Some(REQUEST_TIMEOUT.min(remaining(deadline)?)))?;
    stream.write_all(format!("SHOW {}\n", endpoint.token).as_bytes())?;
    let reply = read_frame(
        &mut stream,
        (Instant::now() + REQUEST_TIMEOUT).min(deadline),
    )?;
    if reply != b"OK\n" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "已有实例未接受窗口唤出请求。",
        ));
    }
    Ok(())
}

fn handle_request(
    mut stream: TcpStream,
    token: &str,
    sender: &SyncSender<()>,
    accepting: &AtomicBool,
) {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let approved = read_frame(&mut stream, deadline)
        .ok()
        .and_then(|frame| {
            frame
                .strip_prefix(b"SHOW ")
                .and_then(|value| value.strip_suffix(b"\n"))
                .map(|value| {
                    value.len() == token.len()
                        && value
                            .iter()
                            .zip(token.as_bytes())
                            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
                            == 0
                })
        })
        .unwrap_or(false);
    // 退出清理期仍持锁，但必须拒绝唤出，让副实例继续等待到锁真正释放。
    let accepted = approved
        && accepting.load(Ordering::Acquire)
        && matches!(sender.try_send(()), Ok(()) | Err(TrySendError::Full(())))
        && accepting.load(Ordering::Acquire);
    if let Ok(timeout) = remaining(deadline) {
        let _ = stream.set_write_timeout(Some(timeout));
        let _ = stream.write_all(if accepted { b"OK\n" } else { b"NO\n" });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    const CHILD_TEST: &str = "single_instance::tests::实例回归子进程入口";

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/instance-tests")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn runtime(&self) -> PathBuf {
            self.0.join("runtime")
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_helper(
        sandbox: &Sandbox,
        name: &str,
        mode: &str,
        binary: Option<&Path>,
    ) -> (ChildGuard, PathBuf) {
        let result = sandbox.0.join(format!("{name}.result"));
        let cwd = sandbox.0.join(format!("cwd-{name}"));
        fs::create_dir_all(&cwd).unwrap();
        let program = binary
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_exe().unwrap());
        let child = Command::new(program)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env("FESILENT_INSTANCE_TEST_MODE", mode)
            .env("FESILENT_INSTANCE_TEST_DIRECTORY", sandbox.runtime())
            .env("FESILENT_INSTANCE_TEST_RESULT", &result)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        (ChildGuard(child), result)
    }

    fn wait_result(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(value) = fs::read_to_string(path)
                && !value.is_empty()
            {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "子进程未发布结果：{}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    #[test]
    fn 实例回归子进程入口() {
        let Ok(mode) = std::env::var("FESILENT_INSTANCE_TEST_MODE") else {
            return;
        };
        let directory =
            PathBuf::from(std::env::var_os("FESILENT_INSTANCE_TEST_DIRECTORY").unwrap());
        let result = PathBuf::from(std::env::var_os("FESILENT_INSTANCE_TEST_RESULT").unwrap());
        if mode == "raw-owner" || mode == "unresponsive-owner" {
            fs::create_dir_all(&directory).unwrap();
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(directory.join("instance.lock"))
                .unwrap();
            file.try_lock().unwrap();
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let endpoint = serde_json::json!({"version":1,"port":listener.local_addr().unwrap().port(),"primary_pid":std::process::id(),"token":"a".repeat(64)});
            if mode == "unresponsive-owner" {
                fs::write(directory.join("instance.json"), endpoint.to_string()).unwrap();
            } else {
                fs::write(directory.join("instance.json"), b"partially-written").unwrap();
            }
            fs::write(&result, "LOCKED").unwrap();
            std::thread::sleep(Duration::from_secs(60));
            drop((file, listener));
            return;
        }
        match acquire_at(&directory, Duration::from_secs(3)).unwrap() {
            Launch::Activated => fs::write(result, "ACTIVATED").unwrap(),
            Launch::Primary(mut owner) => {
                fs::write(&result, "PRIMARY").unwrap();
                let receiver = owner.take_activation_receiver().unwrap();
                let deadline = Instant::now() + Duration::from_secs(60);
                while Instant::now() < deadline {
                    match receiver.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) => fs::write(result.with_extension("activated"), "SHOW").unwrap(),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn 不同工作目录重复启动只激活原进程() {
        let sandbox = Sandbox::new();
        let (_owner, owner_result) = spawn_helper(&sandbox, "owner", "owner", None);
        assert_eq!(wait_result(&owner_result), "PRIMARY");
        let (mut duplicate, duplicate_result) = spawn_helper(&sandbox, "duplicate", "owner", None);
        assert_eq!(
            wait_result(&duplicate_result),
            "ACTIVATED",
            "已有GUI实例时不应再成为主进程"
        );
        assert!(duplicate.0.wait().unwrap().success());
        assert_eq!(
            wait_result(&owner_result.with_extension("activated")),
            "SHOW"
        );
    }

    #[test]
    fn 复制到其他路径的同品牌程序仍只激活原进程() {
        let sandbox = Sandbox::new();
        let (_owner, owner_result) = spawn_helper(&sandbox, "owner", "owner", None);
        assert_eq!(wait_result(&owner_result), "PRIMARY");
        let copy = sandbox.0.join(if cfg!(windows) {
            "another-copy.exe"
        } else {
            "another-copy"
        });
        fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let (mut duplicate, result) = spawn_helper(&sandbox, "copy", "owner", Some(&copy));
        assert_eq!(wait_result(&result), "ACTIVATED");
        assert!(duplicate.0.wait().unwrap().success());
    }

    #[test]
    fn 并发启动竞争只能产生一个主进程() {
        let sandbox = Sandbox::new();
        let children: Vec<_> = (0..6)
            .map(|index| spawn_helper(&sandbox, &format!("race-{index}"), "owner", None))
            .collect();
        let results: Vec<_> = children.iter().map(|(_, path)| wait_result(path)).collect();
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_str() == "PRIMARY")
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_str() == "ACTIVATED")
                .count(),
            5
        );
    }

    #[test]
    fn 崩溃释放锁且旧端口记录不阻止重新启动() {
        let sandbox = Sandbox::new();
        let (mut owner, result) = spawn_helper(&sandbox, "owner", "owner", None);
        assert_eq!(wait_result(&result), "PRIMARY");
        let previous = read_endpoint(&sandbox.runtime()).unwrap().token;
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let Launch::Primary(_replacement) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("崩溃后应能重新获得独占锁")
        };
        assert_ne!(read_endpoint(&sandbox.runtime()).unwrap().token, previous);
    }

    #[test]
    fn 无人持锁时覆盖损坏或过期的元数据() {
        let sandbox = Sandbox::new();
        fs::create_dir_all(sandbox.runtime()).unwrap();
        fs::write(
            sandbox.runtime().join("instance.json"),
            b"stale and invalid",
        )
        .unwrap();
        let Launch::Primary(_owner) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("过期记录不能阻止启动")
        };
        let endpoint: serde_json::Value =
            serde_json::from_slice(&fs::read(sandbox.runtime().join("instance.json")).unwrap())
                .unwrap();
        assert!(endpoint["port"].as_u64().unwrap() > 0);
        assert_eq!(endpoint["token"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn 错误令牌不能唤出窗口且正确请求可随后激活() {
        let sandbox = Sandbox::new();
        let Launch::Primary(mut owner) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("首个实例应该获得锁")
        };
        let receiver = owner.take_activation_receiver().unwrap();
        let endpoint: serde_json::Value =
            serde_json::from_slice(&fs::read(sandbox.runtime().join("instance.json")).unwrap())
                .unwrap();
        let port = endpoint["port"].as_u64().unwrap() as u16;
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .write_all(format!("SHOW {}\n", "0".repeat(64)).as_bytes())
            .unwrap();
        let mut reply = [0_u8; 3];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"NO\n");
        assert!(receiver.try_recv().is_err());
        assert!(matches!(
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap(),
            Launch::Activated
        ));
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn 元数据尚未就绪或激活无应答必须超时而不能另开实例() {
        for mode in ["raw-owner", "unresponsive-owner"] {
            let sandbox = Sandbox::new();
            let (_owner, result) = spawn_helper(&sandbox, "owner", mode, None);
            assert_eq!(wait_result(&result), "LOCKED");
            let start = Instant::now();
            assert!(acquire_at(&sandbox.runtime(), Duration::from_millis(240)).is_err());
            assert!(start.elapsed() < Duration::from_secs(2), "激活等待必须有界");
        }
    }
    #[test]
    fn 只有图形启动受保护而显式调试模式保留多实例() {
        use crate::cli::Args;
        assert!(protects_gui(&Args::default()));
        assert!(protects_gui(&Args {
            tray: true,
            ..Args::default()
        }));
        for args in [
            Args {
                no_tray: true,
                ..Args::default()
            },
            Args {
                loopback: true,
                ..Args::default()
            },
            Args {
                selftest: true,
                ..Args::default()
            },
            Args {
                record: Some("events.jsonl".into()),
                ..Args::default()
            },
            Args {
                replay: Some("events.jsonl".into()),
                ..Args::default()
            },
            Args {
                serve: true,
                ..Args::default()
            },
            Args {
                connect: Some("127.0.0.1:53728".into()),
                ..Args::default()
            },
        ] {
            assert!(!protects_gui(&args));
        }
    }
    #[test]
    #[cfg(unix)]
    fn 唤出凭据仅对当前用户可读写() {
        use std::os::unix::fs::PermissionsExt;
        let sandbox = Sandbox::new();
        let Launch::Primary(_owner) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("首个实例应该获得锁")
        };
        assert_eq!(
            fs::metadata(sandbox.runtime())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for name in ["instance.lock", "instance.json"] {
            assert_eq!(
                fs::metadata(sandbox.runtime().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn 退出清理期拒绝激活且释放锁后原请求能重新启动() {
        let sandbox = Sandbox::new();
        let Launch::Primary(mut owner) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("首个实例应该获得锁")
        };
        let receiver = owner.take_activation_receiver().unwrap();
        owner.activation_accepting().store(false, Ordering::Release);
        let started = Instant::now();
        assert!(
            acquire_at(&sandbox.runtime(), Duration::from_millis(100)).is_err(),
            "退出中的主进程不能吞掉重开请求"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(receiver.try_recv().is_err(), "退出期不能继续投递唤出消息");

        let runtime = sandbox.runtime();
        let retry = std::thread::spawn(move || acquire_at(&runtime, Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(80));
        assert!(!retry.is_finished(), "重开请求应等待旧实例清理释放锁");
        drop(owner);
        let result = retry.join().unwrap().unwrap();
        assert!(
            matches!(result, Launch::Primary(_)),
            "同一重试请求应在旧实例退出后接管"
        );
    }

    #[test]
    fn 静默客户端不能永久阻塞退出() {
        let sandbox = Sandbox::new();
        let Launch::Primary(owner) =
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap()
        else {
            panic!("首个实例应该获得锁")
        };
        let endpoint = read_endpoint(&sandbox.runtime()).unwrap();
        let _idle_client = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port)).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        owner.activation_accepting().store(false, Ordering::Release);
        let started = Instant::now();
        drop(owner);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "静默连接不能挡住退出和释放锁"
        );
        assert!(matches!(
            acquire_at(&sandbox.runtime(), Duration::from_secs(1)).unwrap(),
            Launch::Primary(_)
        ));
    }
}
