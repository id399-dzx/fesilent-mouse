//! QUIC 传输（quinn）：endpoint 建立、连接、消息收发（DEVELOPMENT.md §4.1）。
//!
//! 流划分见 SPEC.md §3：控制/键鼠/剪贴板走可靠流，鼠标移动走 Datagram。
//! 本模块提供 endpoint 与 connection 原语；持久会话循环在 T-204 之上构建。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{
    ClientConfig, Endpoint as QuinnEndpoint, EndpointConfig, ServerConfig, TokioRuntime,
    TransportConfig,
};
use rustls_pki_types::CertificateDer;

use ms_proto::{HEADER_LEN, MAX_PAYLOAD, Msg, decode, encode};

use crate::cert::{Identity, fingerprint_of};
use crate::qos::{DSCP_EF, DscpUdpSocket};
use crate::verify::{AnyClientCert, AnyServerCert};

/// ALPN 协议标识。
const ALPN: &[u8] = b"mouseshare/1";
/// QUIC idle timeout（SPEC §2：8s）。
const IDLE_TIMEOUT: Duration = Duration::from_secs(8);
/// QUIC keep-alive（小于 idle 的一半，维持连接迁移）。
const KEEPALIVE: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("TLS 配置错误: {0}")]
    Tls(String),
    #[error("连接错误: {0}")]
    Connect(String),
    #[error("流错误: {0}")]
    Stream(String),
    #[error("协议错误: {0}")]
    Proto(#[from] ms_proto::ProtoError),
    #[error("发现错误: {0}")]
    Discovery(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NetError>;

fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport_config() -> Arc<TransportConfig> {
    let mut tc = TransportConfig::default();
    if let Ok(t) = IDLE_TIMEOUT.try_into() {
        tc.max_idle_timeout(Some(t));
    }
    tc.keep_alive_interval(Some(KEEPALIVE));
    Arc::new(tc)
}

fn server_config(identity: &Identity) -> Result<ServerConfig> {
    let provider = crypto_provider();
    let verifier = Arc::new(AnyClientCert::new(Arc::clone(&provider)));
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.rustls_cert_chain(), identity.rustls_key())
        .map_err(|e| NetError::Tls(e.to_string()))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qsc = QuicServerConfig::try_from(tls).map_err(|e| NetError::Tls(e.to_string()))?;
    let mut sc = ServerConfig::with_crypto(Arc::new(qsc));
    sc.transport_config(transport_config());
    Ok(sc)
}

fn client_config(identity: &Identity) -> Result<ClientConfig> {
    let provider = crypto_provider();
    let verifier = Arc::new(AnyServerCert::new(Arc::clone(&provider)));
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(identity.rustls_cert_chain(), identity.rustls_key())
        .map_err(|e| NetError::Tls(e.to_string()))?;
    let mut tls = tls;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qcc = QuicClientConfig::try_from(tls).map_err(|e| NetError::Tls(e.to_string()))?;
    let mut cc = ClientConfig::new(Arc::new(qcc));
    cc.transport_config(transport_config());
    Ok(cc)
}

/// QUIC endpoint：同时充当服务端（监听被发现）与客户端（主动连接）。
pub struct Endpoint {
    ep: QuinnEndpoint,
    /// 保留对底层 DSCP socket 的引用，供平台层（Windows qWAVE）取原始句柄。
    /// 仅 Windows 经 `raw_socket()` 读取；其它平台 DSCP 在建 socket 时已生效，故标记允许未用。
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    socket: Arc<DscpUdpSocket>,
}

impl Endpoint {
    /// 在 `bind` 上建立 endpoint（双向能力）。
    ///
    /// 使用带 DSCP 标记的自定义 UDP socket（NFR-08，见 [`crate::qos`]）：全部 QUIC
    /// 流量标记为 EF，便于路由器 QoS / Wi-Fi WMM 优先调度，缓解网络拥塞时的鼠标卡顿。
    /// macOS/Linux 上即时生效；Windows 的 DSCP 由 qWAVE 在连接后补（T-708）。
    pub fn new(bind: SocketAddr, identity: &Identity) -> Result<Self> {
        let socket = DscpUdpSocket::bind(bind, DSCP_EF)?;
        let mut ep = QuinnEndpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            Some(server_config(identity)?),
            socket.clone(),
            Arc::new(TokioRuntime),
        )
        .map_err(|e| NetError::Connect(e.to_string()))?;
        ep.set_default_client_config(client_config(identity)?);
        Ok(Self { ep, socket })
    }

    /// 底层 UDP socket 的原始句柄（Windows：供 qWAVE 标记 DSCP，见 T-708）。
    #[cfg(windows)]
    pub fn raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.socket.raw_socket()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.ep.local_addr().map_err(NetError::Io)
    }

    /// 主动连接对端。
    pub async fn connect(&self, addr: SocketAddr) -> Result<Conn> {
        let connecting = self
            .ep
            .connect(addr, "mouseshare")
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let conn = connecting
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        Ok(Conn { conn })
    }

    /// 接受下一个入站连接（完成握手）。
    pub async fn accept(&self) -> Option<Result<Conn>> {
        let incoming = self.ep.accept().await?;
        Some(match incoming.await {
            Ok(conn) => Ok(Conn { conn }),
            Err(e) => Err(NetError::Connect(e.to_string())),
        })
    }

    pub async fn close(&self) {
        self.ep.close(0u32.into(), b"bye");
        self.ep.wait_idle().await;
    }
}

/// 一条 QUIC 连接。
///
/// `quinn::Connection` 是 `Arc` 句柄，`Clone` 廉价（只增引用计数，共享同一连接）。
/// 派生 `Clone` 以便守护进程把同一连接交给后台流分发任务与剪贴板/文件发送任务共享。
#[derive(Clone)]
pub struct Conn {
    conn: quinn::Connection,
}

impl Conn {
    /// 握手对端证书的 SHA-256 指纹（pinning 用，T-203）。
    pub fn peer_fingerprint(&self) -> Option<[u8; 32]> {
        let identity = self.conn.peer_identity()?;
        let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
        certs.first().map(|c| fingerprint_of(c))
    }

    pub fn remote_addr(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// 当前平滑 RTT 估计（毫秒，饱和到 u32），用于连接质量显示（FR-606）。
    pub fn rtt_ms(&self) -> u32 {
        self.conn.rtt().as_millis().min(u32::MAX as u128) as u32
    }

    /// 导出 TLS 会话密钥材料（RFC 5705），用于 SAS 计算（DEVELOPMENT §4.5）。
    /// 两端用相同 label 导出得到相同密钥。
    pub fn export_keying_material(&self, label: &[u8], len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; len];
        self.conn
            .export_keying_material(&mut out, label, b"")
            .ok()?;
        Some(out)
    }

    /// 计算与对端一致的 6 位配对码（SAS）。需已知本端指纹。
    pub fn compute_sas(&self, own_fp: &[u8; 32]) -> Option<String> {
        let peer_fp = self.peer_fingerprint()?;
        let secret = self.export_keying_material(
            crate::pairing::SAS_EXPORT_LABEL,
            crate::pairing::SAS_EXPORT_LEN,
        )?;
        Some(crate::pairing::compute_sas(&secret, own_fp, &peer_fp))
    }

    /// 经单向流发送一条消息（每消息一流，简单可靠；持久流复用见 T-204）。
    pub async fn send_uni(&self, msg: &Msg) -> Result<()> {
        let bytes = encode(msg)?;
        let mut s = self
            .conn
            .open_uni()
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        s.write_all(&bytes)
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        s.finish().map_err(|e| NetError::Stream(e.to_string()))?;
        s.stopped().await.ok();
        Ok(())
    }

    /// 接受下一条单向流并解出一条消息。
    pub async fn recv_uni(&self) -> Result<Msg> {
        let mut s = self
            .conn
            .accept_uni()
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        let buf = s
            .read_to_end(MAX_PAYLOAD + HEADER_LEN)
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        let (msg, _) = decode(&buf)?;
        Ok(msg)
    }

    /// 经 Datagram 发送（鼠标移动用，不可靠，SPEC §3）。
    pub fn send_datagram(&self, msg: &Msg) -> Result<()> {
        let bytes = encode(msg)?;
        self.conn
            .send_datagram(bytes::Bytes::from(bytes))
            .map_err(|e| NetError::Stream(e.to_string()))
    }

    /// 接收下一个 Datagram 并解码。
    pub async fn recv_datagram(&self) -> Result<Msg> {
        let b = self
            .conn
            .read_datagram()
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        let (msg, _) = decode(&b)?;
        Ok(msg)
    }

    /// 等待连接终止（对端关闭/超时/错误）。重连循环用它检测断连。
    pub async fn wait_closed(&self) {
        let _ = self.conn.closed().await;
    }

    /// 打开一条单向流用于多消息传输（文件传输：每文件一流，SPEC §3）。
    pub async fn open_msg_stream(&self) -> Result<MsgSendStream> {
        let s = self
            .conn
            .open_uni()
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        Ok(MsgSendStream { inner: s })
    }

    /// 接受下一条单向流，按帧读取多条消息。
    pub async fn accept_msg_stream(&self) -> Result<MsgRecvStream> {
        let s = self
            .conn
            .accept_uni()
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        Ok(MsgRecvStream {
            inner: s,
            frame: vec![0; HEADER_LEN],
            received: 0,
        })
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

/// 发送侧持久流：在一条 QUIC 单向流上按帧写多条消息。
pub struct MsgSendStream {
    inner: quinn::SendStream,
}

impl MsgSendStream {
    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        let bytes = encode(msg)?;
        self.inner
            .write_all(&bytes)
            .await
            .map_err(|e| NetError::Stream(e.to_string()))
    }

    /// 正常结束流。
    pub async fn finish(&mut self) -> Result<()> {
        self.inner
            .finish()
            .map_err(|e| NetError::Stream(e.to_string()))?;
        match self.inner.stopped().await {
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(NetError::Stream(format!("对端已停止接收：{code}"))),
            Err(error) => Err(NetError::Stream(error.to_string())),
        }
    }

    /// 取消：重置流，对端读取将报错（用于文件传输取消，FR-503）。
    pub fn reset(&mut self) {
        let _ = self.inner.reset(1u32.into());
    }
}

/// 接收侧持久流：从一条 QUIC 单向流按帧读消息。
pub struct MsgRecvStream {
    inner: quinn::RecvStream,
    frame: Vec<u8>,
    received: usize,
}

impl MsgRecvStream {
    /// 可取消地读一帧。半帧状态保存在流对象，select!定时器不会丢掉已消费字节。
    /// quinn::RecvStream::read本身cancel-safe；read_exact的局部缓冲不具备该保证。
    pub async fn recv(&mut self) -> Result<Option<Msg>> {
        while self.received < HEADER_LEN {
            match self
                .inner
                .read(&mut self.frame[self.received..HEADER_LEN])
                .await
            {
                Ok(Some(count)) => self.received += count,
                Ok(None) if self.received == 0 => return Ok(None),
                Ok(None) => return Err(NetError::Stream("帧头未完成即结束".into())),
                Err(error) => return Err(NetError::Stream(error.to_string())),
            }
        }
        let len = u32::from_le_bytes([self.frame[1], self.frame[2], self.frame[3], self.frame[4]])
            as usize;
        if len > MAX_PAYLOAD {
            return Err(NetError::Stream(format!("帧载荷超限: {len}")));
        }
        self.frame.resize(HEADER_LEN + len, 0);
        while self.received < self.frame.len() {
            match self.inner.read(&mut self.frame[self.received..]).await {
                Ok(Some(count)) => self.received += count,
                Ok(None) => return Err(NetError::Stream("帧载荷未完成即结束".into())),
                Err(error) => return Err(NetError::Stream(error.to_string())),
            }
        }
        let (message, _) = decode(&self.frame)?;
        self.received = 0;
        self.frame.resize(HEADER_LEN, 0);
        Ok(Some(message))
    }
}

#[cfg(test)]
mod cancellation_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn 接收半帧被定时器取消后仍能恢复完整消息() {
        for split in [2, HEADER_LEN + 1] {
            let a_id = Identity::generate().unwrap();
            let b_id = Identity::generate().unwrap();
            let a = Endpoint::new("127.0.0.1:0".parse().unwrap(), &a_id).unwrap();
            let b = Endpoint::new("127.0.0.1:0".parse().unwrap(), &b_id).unwrap();
            let (client, server) = tokio::join!(a.connect(b.local_addr().unwrap()), b.accept());
            let client = client.unwrap();
            let server = server.unwrap().unwrap();
            let message = Msg::from(ms_proto::Ping { seq: 7, t0: 123 });
            let encoded = encode(&message).unwrap();
            let mut send = client.open_msg_stream().await.unwrap();
            send.inner.write_all(&encoded[..split]).await.unwrap();
            let mut receive = server.accept_msg_stream().await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(40), receive.recv())
                    .await
                    .is_err()
            );
            send.inner.write_all(&encoded[split..]).await.unwrap();
            let result = tokio::time::timeout(Duration::from_secs(1), receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                result,
                Some(Msg::Ping(ms_proto::Ping { seq: 7, t0: 123 }))
            ));
            send.send(&Msg::from(ms_proto::Ping { seq: 8, t0: 456 }))
                .await
                .unwrap();
            assert!(matches!(
                receive.recv().await.unwrap(),
                Some(Msg::Ping(ms_proto::Ping { seq: 8, .. }))
            ));
            client.close();
            server.close();
        }
    }
}
