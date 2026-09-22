//! Apple's local USB multiplexer. The plist handshake ends at Result 0;
//! the same connection then carries the app's raw EMLINK stream.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use plist::Value;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub const HEADER_SIZE: usize = 16;
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
pub const APP_PORT: u16 = 9877;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub trait Tunnel: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Tunnel for T {}
pub type BoxedTunnel = Box<dyn Tunnel>;

#[derive(Debug)]
pub enum Error {
    Unavailable(io::Error),
    Io(io::Error),
    Plist(plist::Error),
    Invalid(&'static str),
    Result(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "Apple device service unavailable: {e}"),
            Self::Io(e) => write!(f, "usbmuxd I/O: {e}"),
            Self::Plist(e) => write!(f, "usbmuxd plist: {e}"),
            Self::Invalid(message) => write!(f, "Invalid usbmuxd message: {message}"),
            Self::Result(code) => write!(f, "usbmuxd Result {code}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<plist::Error> for Error {
    fn from(value: plist::Error) -> Self {
        Self::Plist(value)
    }
}

#[derive(Clone, Debug)]
pub enum Endpoint {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

impl Default for Endpoint {
    fn default() -> Self {
        #[cfg(unix)]
        {
            Self::Unix("/var/run/usbmuxd".into())
        }
        #[cfg(not(unix))]
        {
            Self::Tcp(SocketAddr::from(([127, 0, 0, 1], 27015)))
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Request {
    message_type: &'static str,
    client_version_string: &'static str,
    prog_name: &'static str,
    #[serde(rename = "DeviceID", skip_serializing_if = "Option::is_none")]
    device_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port_number: Option<u16>,
}

impl Request {
    pub fn new(message_type: &'static str) -> Self {
        Self {
            message_type,
            client_version_string: concat!("EternalMonitor/", env!("CARGO_PKG_VERSION")),
            prog_name: "EternalMonitor",
            device_id: None,
            port_number: None,
        }
    }

    pub fn connect(device_id: u32, port: u16) -> Self {
        Self {
            device_id: Some(device_id),
            port_number: Some(port.to_be()),
            ..Self::new("Connect")
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceProperties {
    #[serde(default)]
    pub connection_type: String,
    #[serde(default)]
    pub serial_number: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Device {
    #[serde(rename = "DeviceID")]
    pub device_id: u32,
    pub properties: DeviceProperties,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Attached(Device),
    Detached(u32),
}

fn checked_length(header: &[u8; HEADER_SIZE]) -> Result<usize, Error> {
    let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let length = word(0) as usize;
    if !(HEADER_SIZE..=MAX_MESSAGE_SIZE).contains(&length) {
        return Err(Error::Invalid("length"));
    }
    if word(4) != 1 || word(8) != 8 {
        return Err(Error::Invalid("version or message type"));
    }
    Ok(length)
}

pub fn encode_message(tag: u32, body: &impl Serialize) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![0; HEADER_SIZE];
    plist::to_writer_xml(&mut bytes, body)?;
    if bytes.len() > MAX_MESSAGE_SIZE {
        return Err(Error::Invalid("length"));
    }
    let length = bytes.len() as u32;
    bytes[0..4].copy_from_slice(&length.to_le_bytes());
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&8u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&tag.to_le_bytes());
    Ok(bytes)
}

pub async fn read_message(
    stream: &mut (impl AsyncRead + Unpin + ?Sized),
) -> Result<(u32, Value), Error> {
    let mut header = [0; HEADER_SIZE];
    stream.read_exact(&mut header).await?;
    let length = checked_length(&header)?;
    let tag = u32::from_le_bytes(header[12..16].try_into().unwrap());
    let mut body = vec![0; length - HEADER_SIZE];
    stream.read_exact(&mut body).await?;
    Ok((tag, plist::from_bytes(&body)?))
}

fn result_code(value: &Value) -> Result<u32, Error> {
    let dictionary = value.as_dictionary().ok_or(Error::Invalid("dictionary"))?;
    if dictionary.get("MessageType").and_then(Value::as_string) != Some("Result") {
        return Err(Error::Invalid("expected Result"));
    }
    dictionary
        .get("Number")
        .and_then(Value::as_unsigned_integer)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(Error::Invalid("result number"))
}

pub struct Client {
    endpoint: Endpoint,
    next_tag: AtomicU32,
}

impl Client {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            next_tag: AtomicU32::new(1),
        }
    }

    async fn open(&self) -> Result<BoxedTunnel, Error> {
        let connect = async {
            let tunnel: BoxedTunnel = match &self.endpoint {
                Endpoint::Tcp(address) => {
                    let socket = TcpStream::connect(address).await?;
                    socket.set_nodelay(true)?;
                    let _ = socket2::SockRef::from(&socket).set_send_buffer_size(32 * 1024);
                    Box::new(socket)
                }
                #[cfg(unix)]
                Endpoint::Unix(path) => Box::new(tokio::net::UnixStream::connect(path).await?),
            };
            Ok::<_, io::Error>(tunnel)
        };
        timeout(CONNECT_TIMEOUT, connect)
            .await
            .map_err(|_| {
                Error::Unavailable(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "service connect timeout",
                ))
            })?
            .map_err(Error::Unavailable)
    }

    async fn request(&self, request: &Request) -> Result<(BoxedTunnel, Value), Error> {
        let mut stream = self.open().await?;
        let tag = self.next_tag.fetch_add(1, Ordering::Relaxed);
        let bytes = encode_message(tag, request)?;
        let exchange = async {
            stream.write_all(&bytes).await?;
            let (returned_tag, value) = read_message(&mut *stream).await?;
            if tag != returned_tag {
                return Err(Error::Invalid("reply tag"));
            }
            Ok(value)
        };
        let value = timeout(REQUEST_TIMEOUT, exchange).await.map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "service reply timeout",
            ))
        })??;
        Ok((stream, value))
    }

    pub async fn list_devices(&self) -> Result<Vec<Device>, Error> {
        let (_, value) = self.request(&Request::new("ListDevices")).await?;
        let dictionary = value
            .as_dictionary()
            .ok_or(Error::Invalid("device dictionary"))?;
        let list = dictionary
            .get("DeviceList")
            .and_then(Value::as_array)
            .ok_or(Error::Invalid("DeviceList"))?;
        let devices: Result<Vec<Device>, _> = list.iter().map(plist::from_value).collect();
        Ok(devices?
            .into_iter()
            .filter(|d| d.properties.connection_type == "USB")
            .collect())
    }

    pub async fn connect_device(&self, device_id: u32, port: u16) -> Result<BoxedTunnel, Error> {
        let (stream, value) = self.request(&Request::connect(device_id, port)).await?;
        match result_code(&value)? {
            0 => Ok(stream),
            code => Err(Error::Result(code)),
        }
    }

    pub async fn listen(&self) -> Result<Events, Error> {
        let (stream, value) = self.request(&Request::new("Listen")).await?;
        match result_code(&value)? {
            0 => Ok(Events { stream }),
            code => Err(Error::Result(code)),
        }
    }
}

pub struct Events {
    stream: BoxedTunnel,
}

impl Events {
    pub async fn next_event(&mut self) -> Result<DeviceEvent, Error> {
        let (_, value) = read_message(&mut *self.stream).await?;
        let dictionary = value
            .as_dictionary()
            .ok_or(Error::Invalid("event dictionary"))?;
        match dictionary.get("MessageType").and_then(Value::as_string) {
            Some("Attached") => Ok(DeviceEvent::Attached(plist::from_value(&value)?)),
            Some("Detached") => {
                let id = dictionary
                    .get("DeviceID")
                    .and_then(Value::as_unsigned_integer)
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or(Error::Invalid("DeviceID"))?;
                Ok(DeviceEvent::Detached(id))
            }
            _ => Err(Error::Invalid("event type")),
        }
    }
}
