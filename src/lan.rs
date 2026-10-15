//! Opt-in LAN discovery and TLS 1.3. Discovery supplies the selected endpoint's
//! certificate fingerprint automatically; recipient consent authorizes each song.
use crate::library::Result;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const DISCOVERY_PORT: u16 = 45881;
const MAX_PEERS: usize = 32;
fn local(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if ip.is_private() || ip.is_link_local() || ip.is_loopback())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn fingerprint(cert: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, cert).as_ref())
}
fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
fn session_name(cert: &[u8]) -> String {
    // A friendly session alias rather than an account name or computer hostname.
    const FIRST: [&str; 16] = [
        "Quiet", "Bright", "Gentle", "Calm", "Swift", "Soft", "Warm", "Clear", "Little", "Sunny",
        "Silver", "Golden", "Happy", "Mellow", "Fresh", "Cosy",
    ];
    const SECOND: [&str; 16] = [
        "Coral", "Amber", "Jade", "Blue", "Ivory", "Ruby", "Pearl", "Indigo", "Rose", "Mint",
        "Violet", "Copper", "Teal", "Olive", "Aqua", "Lilac",
    ];
    const THIRD: [&str; 16] = [
        "Cedar", "Piano", "Falcon", "Comet", "Harp", "Willow", "Finch", "Maple", "Echo", "Orchid",
        "Robin", "Ocean", "Sparrow", "Melody", "River", "Moon",
    ];
    let hash = digest::digest(&digest::SHA256, cert);
    let bytes = hash.as_ref();
    format!(
        "{} {} {}",
        FIRST[(bytes[0] & 15) as usize],
        SECOND[(bytes[1] & 15) as usize],
        THIRD[(bytes[2] & 15) as usize]
    )
}
#[derive(Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub address: SocketAddr,
    pub fingerprint: String,
}

/// Cancellation-aware TCP shared by both TLS roles. Timeouts never return
/// Interrupted (read_exact retries that indefinitely).
pub struct Socket {
    tcp: TcpStream,
    cancel: Arc<AtomicBool>,
    until: Instant,
}
impl Socket {
    fn new(tcp: TcpStream, cancel: Arc<AtomicBool>) -> io::Result<Self> {
        tcp.set_read_timeout(Some(Duration::from_millis(200)))?;
        tcp.set_write_timeout(Some(Duration::from_millis(200)))?;
        Ok(Self {
            tcp,
            cancel,
            until: Instant::now() + Duration::from_secs(900),
        })
    }
    fn check(&self, idle: Instant) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Sharing cancelled.",
            ));
        }
        if Instant::now() > self.until || idle.elapsed() > Duration::from_secs(75) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Sharing session timed out.",
            ));
        }
        Ok(())
    }
}
impl Read for Socket {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        let idle = Instant::now();
        loop {
            self.check(idle)?;
            match self.tcp.read(data) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}
impl Write for Socket {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let idle = Instant::now();
        loop {
            self.check(idle)?;
            match self.tcp.write(data) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

#[derive(Debug)]
struct Pinned {
    key: String,
    provider: Arc<CryptoProvider>,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if fingerprint(cert.as_ref()) != self.key {
            return Err(rustls::Error::General(
                "Device certificate changed. Refresh nearby devices and try again. No song was sent.".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            signed,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            signed,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub type Sender = StreamOwned<ClientConnection, Socket>;
pub type Receiver = StreamOwned<ServerConnection, Socket>;
pub fn connect(peer: &Peer, cancel: Arc<AtomicBool>) -> Result<Sender> {
    if !local(peer.address.ip())
        || peer.address.port() == 0
        || !valid_fingerprint(&peer.fingerprint)
    {
        return Err("Choose a device on your local network.".into());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            key: peer.fingerprint.to_ascii_lowercase(),
            provider,
        }))
        .with_no_client_auth();
    let mut connection =
        ClientConnection::new(Arc::new(config), ServerName::try_from("monolith.local")?)?;
    connection.set_buffer_limit(Some(128 * 1024));
    let socket = TcpStream::connect_timeout(&peer.address, Duration::from_secs(3))?;
    Ok(StreamOwned::new(connection, Socket::new(socket, cancel)?))
}

pub struct Listener {
    tcp: TcpListener,
    discovery: Option<UdpSocket>,
    config: Arc<ServerConfig>,
    fingerprint: String,
    pub name: String,
    cancel: Arc<AtomicBool>,
}
impl Listener {
    pub fn bind(cancel: Arc<AtomicBool>) -> Result<Self> {
        Self::bind_at("0.0.0.0:0", true, cancel)
    }
    fn bind_at(address: &str, advertise: bool, cancel: Arc<AtomicBool>) -> Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec!["monolith.local".into()])?;
        let fingerprint = fingerprint(certified.cert.der().as_ref());
        let name = session_name(certified.cert.der().as_ref());
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_no_client_auth()
                .with_single_cert(
                    vec![certified.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into(),
                )?;
        let tcp = TcpListener::bind(address)?;
        tcp.set_nonblocking(true)?;
        let discovery = if advertise {
            let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))?;
            udp.set_nonblocking(true)?;
            Some(udp)
        } else {
            None
        };
        Ok(Self {
            tcp,
            discovery,
            config: Arc::new(config),
            fingerprint,
            name,
            cancel,
        })
    }
    pub fn accept(&self) -> Result<Option<Receiver>> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        // Bound work per tick even if a peer floods discovery packets.
        if let Some(udp) = &self.discovery {
            let mut buffer = [0; 2048];
            for _ in 0..8 {
                match udp.recv_from(&mut buffer) {
                    Ok((n, source)) if local(source.ip()) => {
                        if let Ok(v) = serde_json::from_slice::<Value>(&buffer[..n]) {
                            if v["app"] == "monolith-music"
                                && v["version"] == 1
                                && v["type"] == "discover"
                                && v["nonce"].as_str().is_some_and(|s| {
                                    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
                                })
                            {
                                let reply = json!({"app":"monolith-music","version":1,"type":"available","nonce":v["nonce"],"port":self.tcp.local_addr()?.port(),"alias":self.name,"fingerprint":self.fingerprint});
                                let _ = udp.send_to(&serde_json::to_vec(&reply)?, source);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        match self.tcp.accept() {
            Ok((tcp, source)) if local(source.ip()) => {
                let mut connection = ServerConnection::new(self.config.clone())?;
                connection.set_buffer_limit(Some(128 * 1024));
                Ok(Some(StreamOwned::new(
                    connection,
                    Socket::new(tcp, self.cancel.clone())?,
                )))
            }
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

pub fn discover(cancel: &AtomicBool) -> Result<Vec<Peer>> {
    discover_at(
        SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)),
        cancel,
    )
}
fn parse_peer(v: &Value, source: SocketAddr, nonce: &str) -> Option<Peer> {
    if !local(source.ip())
        || v["app"] != "monolith-music"
        || v["version"] != 1
        || v["type"] != "available"
        || v["nonce"] != nonce
    {
        return None;
    }
    let port = v["port"].as_u64().filter(|p| *p > 0 && *p <= 65535)?;
    let fingerprint = v["fingerprint"].as_str().filter(|s| valid_fingerprint(s))?;
    let alias = v["alias"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 64 && !s.chars().any(char::is_control))?;
    Some(Peer {
        name: format!("{} · {}", alias.trim(), source.ip()),
        address: SocketAddr::new(source.ip(), port as u16),
        fingerprint: fingerprint.to_ascii_lowercase(),
    })
}
fn discover_at(address: SocketAddr, cancel: &AtomicBool) -> Result<Vec<Peer>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_broadcast(true)?;
    socket.set_ttl(1)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut nonce = [0; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "Cannot generate discovery nonce.")?;
    let nonce = hex(&nonce);
    let query = serde_json::to_vec(
        &json!({"app":"monolith-music","version":1,"type":"discover","nonce":nonce}),
    )?;
    socket.send_to(&query, address)?;
    let until = Instant::now() + Duration::from_secs(2);
    let mut peers = Vec::new();
    let mut buffer = [0; 2048];
    while Instant::now() < until && !cancel.load(Ordering::Relaxed) && peers.len() < MAX_PEERS {
        match socket.recv_from(&mut buffer) {
            Ok((n, source)) if local(source.ip()) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&buffer[..n]) {
                    if let Some(peer) = parse_peer(&v, source, &nonce) {
                        if !peers.iter().any(|p: &Peer| p.address == peer.address) {
                            peers.push(peer);
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovered_tls_roundtrip_wrong_certificate_and_cancellation() -> Result<()> {
        for correct in [true, false] {
            let cancel = Arc::new(AtomicBool::new(false));
            let server = Listener::bind_at("127.0.0.1:0", false, cancel.clone())?;
            let peer = Peer {
                name: "test".into(),
                address: server.tcp.local_addr()?,
                fingerprint: if correct {
                    server.fingerprint.clone()
                } else {
                    "0".repeat(64)
                },
            };
            let thread = std::thread::spawn(move || {
                let mut stream = loop {
                    if let Some(s) = server.accept().unwrap() {
                        break s;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                let mut data = [0; 4];
                let result = stream.read_exact(&mut data);
                if correct {
                    result.unwrap();
                    assert_eq!(&data, b"song");
                    stream.write_all(b"saved").unwrap();
                    stream.flush().unwrap();
                } else {
                    assert!(result.is_err());
                }
            });
            let mut client = connect(&peer, cancel.clone())?;
            let result = client.write_all(b"song").and_then(|_| client.flush());
            if correct {
                result?;
                let mut answer = [0; 5];
                client.read_exact(&mut answer)?;
                assert_eq!(&answer, b"saved");
                cancel.store(true, Ordering::Relaxed);
                assert!(client.read(&mut answer).is_err());
            } else {
                assert!(result.is_err());
            }
            drop(client);
            thread.join().unwrap();
        }
        assert!(!valid_fingerprint("1234"));
        assert!(!local("8.8.8.8".parse()?));
        Ok(())
    }
    #[test]
    fn discovery_is_scoped_and_tls_imports_two_real_songs() -> Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        let discovery_address = udp.local_addr()?;
        udp.set_nonblocking(true)?;
        let mut server = Listener::bind_at("127.0.0.1:0", false, cancel.clone())?;
        server.discovery = Some(udp);
        let expected = server.tcp.local_addr()?;
        let expected_fingerprint = server.fingerprint.clone();
        let expected_name = server.name.clone();
        let dir = tempfile::tempdir()?;
        let destination = dir.path().join("received");
        let receiver_destination = destination.clone();
        let server_cancel = cancel.clone();
        let thread = std::thread::spawn(move || {
            let db = crate::library::Library::open(&receiver_destination.with_extension("sqlite3"))
                .unwrap();
            for _ in 0..2 {
                let mut stream = loop {
                    if let Some(s) = server.accept().unwrap() {
                        break s;
                    }
                    assert!(!server_cancel.load(Ordering::Relaxed));
                    std::thread::sleep(Duration::from_millis(10));
                };
                crate::nearby::receive(
                    &mut stream,
                    &receiver_destination,
                    &server_cancel,
                    |_| true,
                    |_, _| {},
                    |path| {
                        db.import(path)?;
                        Ok(())
                    },
                )
                .unwrap();
            }
            assert_eq!(db.tracks(0).unwrap().len(), 2);
        });
        let peers = discover_at(discovery_address, &cancel)?;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, expected);
        assert_eq!(peers[0].fingerprint, expected_fingerprint);
        assert!(peers[0].name.starts_with(&expected_name));
        let mut wav = b"RIFF".to_vec();
        wav.extend(16036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(16000u32.to_le_bytes());
        wav.resize(16044, 0);
        for name in ["one.wav", "two.wav"] {
            let source = dir.path().join(name);
            std::fs::write(&source, &wav)?;
            let offer = crate::nearby::Offer::from_file(&source, "Example")?;
            let mut stream = connect(&peers[0], cancel.clone())?;
            crate::nearby::send(&mut stream, &source, &offer, &cancel, |_, _| {})?;
        }
        thread.join().unwrap();
        for name in ["one.wav", "two.wav"] {
            assert_eq!(std::fs::read(destination.join(name))?, wav);
        }
        Ok(())
    }
    #[test]
    fn discovery_rejects_unusable_certificates_and_spoofed_replies() {
        let nonce = "a".repeat(32);
        let source = "127.0.0.1:45881".parse().unwrap();
        let valid = json!({"app":"monolith-music","version":1,"type":"available","nonce":nonce,"port":1234,"alias":"Quiet Coral Cedar","fingerprint":"b".repeat(64)});
        assert!(parse_peer(&valid, source, &nonce).is_some());
        for (key, value) in [
            ("nonce", json!("wrong")),
            ("app", json!("another-app")),
            ("port", json!(0)),
            ("fingerprint", json!("bad")),
            ("alias", json!("bad\nname")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(parse_peer(&invalid, source, &nonce).is_none());
        }
    }
}
