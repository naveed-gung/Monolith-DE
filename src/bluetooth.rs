//! Native RFCOMM transport. No network server or discoverability changes.
use crate::library::Result;
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Peer {
    pub address: u64,
    pub name: String,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::{
        Win32::{
            Devices::Bluetooth::*,
            Foundation::{BOOL, CloseHandle, HANDLE},
            Networking::WinSock::*,
        },
        core::{GUID, PSTR, PWSTR},
    };
    const UUID: GUID = GUID::from_u128(0xe8793c34_7c2e_49ad_88c2_16c9a0762c91);
    const FAMILY: u16 = 32; // AF_BTH (ws2bth.h)
    fn error() -> io::Error {
        io::Error::from_raw_os_error(unsafe { WSAGetLastError().0 })
    }
    fn ok(value: i32) -> io::Result<()> {
        if value == SOCKET_ERROR {
            Err(error())
        } else {
            Ok(())
        }
    }
    pub struct Session;
    impl Session {
        pub fn new() -> Result<Self> {
            let mut data = WSADATA::default();
            let code = unsafe { WSAStartup(0x0202, &mut data) };
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code).into());
            }
            let session = Self;
            let params = BLUETOOTH_FIND_RADIO_PARAMS {
                dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
            };
            let mut radio = HANDLE::default();
            match unsafe { BluetoothFindFirstRadio(&params, &mut radio) } {
                Ok(find) => unsafe { let _ = BluetoothFindRadioClose(find); let _ = CloseHandle(radio); },
                Err(_) => return Err("No Bluetooth radio available. Enable Bluetooth or connect an adapter, then refresh.".into()),
            }
            Ok(session)
        }
        pub fn peers(&self) -> Result<Vec<Peer>> {
            let params = BLUETOOTH_DEVICE_SEARCH_PARAMS {
                dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
                fReturnAuthenticated: BOOL(1),
                ..Default::default()
            };
            let mut info = BLUETOOTH_DEVICE_INFO {
                dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
                ..Default::default()
            };
            let mut peers = Vec::new();
            if let Ok(find) = unsafe { BluetoothFindFirstDevice(&params, &mut info) } {
                loop {
                    if info.fAuthenticated.as_bool() {
                        let name = String::from_utf16_lossy(
                            &info.szName[..info
                                .szName
                                .iter()
                                .position(|c| *c == 0)
                                .unwrap_or(info.szName.len())],
                        );
                        peers.push(Peer {
                            address: unsafe { info.Address.Anonymous.ullLong },
                            name: name.chars().filter(|c| !c.is_control()).take(80).collect(),
                        });
                    }
                    if unsafe { BluetoothFindNextDevice(find, &mut info) }.is_err() {
                        break;
                    }
                    if peers.len() >= 64 {
                        break;
                    }
                }
                unsafe {
                    let _ = BluetoothFindDeviceClose(find);
                }
            }
            Ok(peers)
        }
        pub fn connect(&self, peer: &Peer, cancel: Arc<AtomicBool>) -> Result<Stream> {
            let stream = Stream::new(cancel)?;
            let address = SOCKADDR_BTH {
                addressFamily: FAMILY,
                btAddr: peer.address,
                serviceClassId: UUID,
                port: 0,
            };
            let result = unsafe {
                connect(
                    stream.socket,
                    &address as *const _ as _,
                    std::mem::size_of_val(&address) as i32,
                )
            };
            if result == SOCKET_ERROR && unsafe { WSAGetLastError() } != WSAEWOULDBLOCK {
                return Err(error().into());
            }
            stream.wait(false, Duration::from_secs(15))?;
            let mut code = 0i32;
            let mut size = 4;
            ok(unsafe {
                getsockopt(
                    stream.socket,
                    SOL_SOCKET,
                    SO_ERROR,
                    PSTR(&mut code as *mut _ as _),
                    &mut size,
                )
            })?;
            if code != 0 {
                return Err(format!("Cannot reach Monolith on this paired device (Bluetooth error {code}). Open Share music and Receive on the other device.").into());
            }
            Ok(stream)
        }
        pub fn listen(&self, cancel: Arc<AtomicBool>) -> Result<Listener> {
            let stream = Stream::new(cancel)?;
            let mut address = SOCKADDR_BTH {
                addressFamily: FAMILY,
                port: u32::MAX,
                ..Default::default()
            };
            ok(unsafe {
                bind(
                    stream.socket,
                    &address as *const _ as _,
                    std::mem::size_of_val(&address) as i32,
                )
            })?;
            ok(unsafe { listen(stream.socket, 1) })?;
            let mut size = std::mem::size_of_val(&address) as i32;
            ok(unsafe { getsockname(stream.socket, &mut address as *mut _ as _, &mut size) })?;
            let listener = Listener { stream, address };
            listener.register(RNRSERVICE_REGISTER)?;
            Ok(listener)
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                WSACleanup();
            }
        }
    }
    pub struct Stream {
        socket: SOCKET,
        cancel: Arc<AtomicBool>,
        expires: Instant,
    }
    impl Stream {
        fn new(cancel: Arc<AtomicBool>) -> io::Result<Self> {
            let socket = unsafe { socket(FAMILY as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32) };
            if socket == INVALID_SOCKET {
                return Err(error());
            }
            let stream = Self {
                socket,
                cancel,
                expires: Instant::now() + Duration::from_secs(15 * 60),
            };
            // Both must succeed; never silently fall back to an unencrypted connection.
            for option in [SO_BTH_AUTHENTICATE, SO_BTH_ENCRYPT] {
                ok(unsafe {
                    setsockopt(
                        socket,
                        SOL_RFCOMM as i32,
                        option as i32,
                        Some(&1u32.to_ne_bytes()),
                    )
                })?;
            }
            stream.nonblocking()?;
            Ok(stream)
        }
        fn nonblocking(&self) -> io::Result<()> {
            let mut yes = 1;
            ok(unsafe { ioctlsocket(self.socket, FIONBIO, &mut yes) })
        }
        fn wait(&self, read: bool, timeout: Duration) -> io::Result<()> {
            let until = (Instant::now() + timeout).min(self.expires);
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Sharing cancelled.",
                    ));
                }
                if Instant::now() >= until {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Bluetooth sharing timed out.",
                    ));
                }
                let mut set = FD_SET::default();
                set.fd_count = 1;
                set.fd_array[0] = self.socket;
                let mut except = set;
                let timeout = TIMEVAL {
                    tv_sec: 0,
                    tv_usec: 100_000,
                };
                let code = unsafe {
                    select(
                        0,
                        if read { Some(&mut set) } else { None },
                        if read { None } else { Some(&mut set) },
                        Some(&mut except),
                        Some(&timeout),
                    )
                };
                ok(code)?;
                if except.fd_count > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Bluetooth connection failed.",
                    ));
                }
                if set.fd_count > 0 {
                    return Ok(());
                }
            }
        }
    }
    impl Read for Stream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            self.wait(true, Duration::from_secs(75))?;
            let count = unsafe { recv(self.socket, buffer, SEND_RECV_FLAGS(0)) };
            ok(count)?;
            Ok(count as usize)
        }
    }
    impl Write for Stream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            self.wait(false, Duration::from_secs(75))?;
            let count = unsafe { send(self.socket, buffer, SEND_RECV_FLAGS(0)) };
            ok(count)?;
            Ok(count as usize)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                closesocket(self.socket);
            }
        }
    }
    pub struct Listener {
        stream: Stream,
        address: SOCKADDR_BTH,
    }
    impl Listener {
        fn register(&self, action: WSAESETSERVICEOP) -> io::Result<()> {
            let mut uuid = UUID;
            let mut address = self.address;
            let mut service_name: Vec<u16> = "Monolith Music\0".encode_utf16().collect();
            let mut info = CSADDR_INFO {
                LocalAddr: SOCKET_ADDRESS {
                    lpSockaddr: &mut address as *mut _ as _,
                    iSockaddrLength: std::mem::size_of_val(&address) as i32,
                },
                iSocketType: SOCK_STREAM.0,
                iProtocol: BTHPROTO_RFCOMM as i32,
                ..Default::default()
            };
            let query = WSAQUERYSETW {
                dwSize: std::mem::size_of::<WSAQUERYSETW>() as u32,
                lpszServiceInstanceName: PWSTR(service_name.as_mut_ptr()),
                lpServiceClassId: &mut uuid,
                dwNameSpace: NS_BTH,
                dwNumberOfCsAddrs: 1,
                lpcsaBuffer: &mut info,
                ..Default::default()
            };
            ok(unsafe { WSASetServiceW(&query, action, 0) })
        }
        pub fn accept(&self) -> Result<Option<(Stream, String)>> {
            let mut address = SOCKADDR_BTH::default();
            let mut size = std::mem::size_of_val(&address) as i32;
            let socket = unsafe {
                accept(
                    self.stream.socket,
                    Some(&mut address as *mut _ as _),
                    Some(&mut size),
                )
            };
            if socket == INVALID_SOCKET {
                if unsafe { WSAGetLastError() } == WSAEWOULDBLOCK {
                    return Ok(None);
                }
                return Err(error().into());
            }
            let stream = Stream {
                socket,
                cancel: self.stream.cancel.clone(),
                expires: Instant::now() + Duration::from_secs(15 * 60),
            };
            stream.nonblocking()?;
            let mut info = BLUETOOTH_DEVICE_INFO {
                dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
                ..Default::default()
            };
            info.Address.Anonymous.ullLong = address.btAddr;
            if unsafe { BluetoothGetDeviceInfo(HANDLE::default(), &mut info) } != 0
                || !info.fAuthenticated.as_bool()
            {
                return Err(
                    "Incoming connection must come from an authenticated paired device.".into(),
                );
            }
            let name = String::from_utf16_lossy(
                &info.szName[..info
                    .szName
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(info.szName.len())],
            );
            Ok(Some((
                stream,
                name.chars().filter(|c| !c.is_control()).take(80).collect(),
            )))
        }
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = self.register(RNRSERVICE_DELETE);
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            net::{TcpListener, TcpStream},
            os::windows::io::IntoRawSocket,
        };
        #[test]
        fn native_socket_wait_cancels_and_times_out_without_bluetooth_hardware() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let _server = listener.accept().unwrap().0;
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut stream = Stream {
                socket: SOCKET(client.into_raw_socket() as usize),
                cancel: cancelled.clone(),
                expires: Instant::now() + Duration::from_secs(5),
            };
            stream.nonblocking().unwrap();
            assert_eq!(
                stream.wait(true, Duration::ZERO).unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            let worker = std::thread::spawn(move || {
                let started = Instant::now();
                let error = stream.read_exact(&mut [0; 4]).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
                assert!(started.elapsed() < Duration::from_secs(2));
            });
            std::thread::sleep(Duration::from_millis(20));
            cancelled.store(true, Ordering::Relaxed);
            worker.join().unwrap();
        }
    }
}
#[cfg(windows)]
pub use platform::*;

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct Session;
    pub struct Stream;
    pub struct Listener;
    fn unavailable<T>() -> Result<T> {
        Err("Bluetooth transport currently requires Windows. Linux BlueZ and iPhone adapters are not implemented yet.".into())
    }
    impl Session {
        pub fn new() -> Result<Self> {
            unavailable()
        }
        pub fn peers(&self) -> Result<Vec<Peer>> {
            unavailable()
        }
        pub fn connect(&self, _: &Peer, _: Arc<AtomicBool>) -> Result<Stream> {
            unavailable()
        }
        pub fn listen(&self, _: Arc<AtomicBool>) -> Result<Listener> {
            unavailable()
        }
    }
    impl Listener {
        pub fn accept(&self) -> Result<Option<(Stream, String)>> {
            unavailable()
        }
    }
    impl Read for Stream {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    impl Write for Stream {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Unsupported.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
#[cfg(not(windows))]
pub use platform::*;
