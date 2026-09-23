use std::net::SocketAddr;

use eternal_host::transport::usbmuxd::{encode_message, read_message};
use plist::{Dictionary, Value};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

pub enum Step {
    List,
    Listen,
    Connect {
        result: u32,
        tunnel: Option<SocketAddr>,
    },
    WrongTag,
}

fn object(fields: &[(&str, Value)]) -> Value {
    let mut dictionary = Dictionary::new();
    for (key, value) in fields {
        dictionary.insert((*key).into(), value.clone());
    }
    Value::Dictionary(dictionary)
}

pub fn device(id: u32, connection: &str) -> Value {
    object(&[
        ("DeviceID", id.into()),
        (
            "Properties",
            object(&[
                ("ConnectionType", connection.into()),
                ("SerialNumber", "test-device".into()),
            ]),
        ),
    ])
}

async fn reply(socket: &mut TcpStream, tag: u32, value: Value) {
    // Deliberately split every field, including the header's length word.
    for chunk in encode_message(tag, &value).unwrap().chunks(3) {
        socket.write_all(chunk).await.unwrap();
        tokio::task::yield_now().await;
    }
}

pub async fn serve(listener: TcpListener, steps: Vec<Step>) {
    for step in steps {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (tag, value) = read_message(&mut socket).await.unwrap();
        let request = value.as_dictionary().unwrap();
        assert_eq!(request["ProgName"].as_string(), Some("EternalMonitor"));
        assert!(request["ClientVersionString"]
            .as_string()
            .unwrap()
            .starts_with("EternalMonitor/"));
        match step {
            Step::List => {
                assert_eq!(request["MessageType"].as_string(), Some("ListDevices"));
                reply(
                    &mut socket,
                    tag,
                    object(&[(
                        "DeviceList",
                        Value::Array(vec![device(17, "USB"), device(18, "Network")]),
                    )]),
                )
                .await;
            }
            Step::Listen => {
                assert_eq!(request["MessageType"].as_string(), Some("Listen"));
                reply(
                    &mut socket,
                    tag,
                    object(&[("MessageType", "Result".into()), ("Number", 0u32.into())]),
                )
                .await;
                let mut attached = device(17, "USB");
                attached
                    .as_dictionary_mut()
                    .unwrap()
                    .insert("MessageType".into(), "Attached".into());
                reply(&mut socket, 0, attached).await;
                reply(
                    &mut socket,
                    0,
                    object(&[
                        ("MessageType", "Detached".into()),
                        ("DeviceID", 17u32.into()),
                    ]),
                )
                .await;
            }
            Step::Connect { result, tunnel } => {
                assert_eq!(request["MessageType"].as_string(), Some("Connect"));
                assert_eq!(request["DeviceID"].as_unsigned_integer(), Some(17));
                assert_eq!(request["PortNumber"].as_unsigned_integer(), Some(38182));
                reply(
                    &mut socket,
                    tag,
                    object(&[("MessageType", "Result".into()), ("Number", result.into())]),
                )
                .await;
                if let Some(target) = tunnel {
                    let mut upstream = TcpStream::connect(target).await.unwrap();
                    tokio::io::copy_bidirectional(&mut socket, &mut upstream)
                        .await
                        .unwrap();
                }
            }
            Step::WrongTag => {
                reply(
                    &mut socket,
                    tag + 1,
                    object(&[("DeviceList", Value::Array(vec![]))]),
                )
                .await;
            }
        }
    }
}
