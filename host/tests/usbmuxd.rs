use std::time::Duration;

use eternal_host::transport::usbmuxd::{self, Client, DeviceEvent, Endpoint, Error, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[path = "support/fake_usbmuxd.rs"]
mod fake;

#[tokio::test]
async fn plist_header_and_swapped_port_round_trip_across_partial_reads() {
    let bytes = usbmuxd::encode_message(0x1234_5678, &Request::connect(17, 9877)).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(
        &bytes[4..16],
        &[1, 0, 0, 0, 8, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]
    );
    let (mut writer, mut reader) = tokio::io::duplex(8);
    let sending = tokio::spawn(async move {
        writer.write_all(&bytes).await.unwrap();
    });
    let (tag, value) = usbmuxd::read_message(&mut reader).await.unwrap();
    assert_eq!(tag, 0x1234_5678);
    assert_eq!(
        value.as_dictionary().unwrap()["PortNumber"].as_unsigned_integer(),
        Some(38182)
    );
    sending.await.unwrap();
}

#[tokio::test]
async fn oversized_length_and_wrong_header_are_rejected_before_body_read() {
    for (length, version, kind) in [
        (15u32, 1u32, 8u32),
        (1_048_577, 1, 8),
        (32, 0, 8),
        (32, 1, 7),
    ] {
        let mut bytes = Vec::new();
        for value in [length, version, kind, 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let error = usbmuxd::read_message(&mut bytes.as_slice())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Invalid(_)));
    }
}

#[tokio::test]
async fn fake_service_lists_listens_reports_errors_and_switches_to_echo_tunnel() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_address = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        let (socket, _) = echo.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        tokio::io::copy(&mut reader, &mut writer).await.unwrap();
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(Endpoint::Tcp(listener.local_addr().unwrap()));
    let server = tokio::spawn(fake::serve(
        listener,
        vec![
            fake::Step::List,
            fake::Step::Listen,
            fake::Step::Connect {
                result: 2,
                tunnel: None,
            },
            fake::Step::Connect {
                result: 3,
                tunnel: None,
            },
            fake::Step::Connect {
                result: 5,
                tunnel: None,
            },
            fake::Step::Connect {
                result: 0,
                tunnel: Some(echo_address),
            },
            fake::Step::WrongTag,
        ],
    ));
    let devices = client.list_devices().await.unwrap();
    assert_eq!(devices.len(), 1, "network devices are not USB tunnels");
    assert_eq!(devices[0].device_id, 17);
    let mut events = client.listen().await.unwrap();
    assert_eq!(
        events.next_event().await.unwrap(),
        DeviceEvent::Attached(devices[0].clone())
    );
    assert_eq!(
        events.next_event().await.unwrap(),
        DeviceEvent::Detached(17)
    );
    drop(events);
    for expected in [2, 3, 5] {
        assert!(
            matches!(client.connect_device(17, usbmuxd::APP_PORT).await, Err(Error::Result(code)) if code == expected)
        );
    }
    let mut tunnel = client.connect_device(17, usbmuxd::APP_PORT).await.unwrap();
    tunnel
        .write_all(b"raw tunnel after Result 0")
        .await
        .unwrap();
    let mut echoed = [0; 25];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"raw tunnel after Result 0");
    tunnel.shutdown().await.unwrap();
    drop(tunnel);
    echo_task.await.unwrap();
    assert!(matches!(
        client.list_devices().await,
        Err(Error::Invalid("reply tag"))
    ));
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn closed_service_is_reported_as_unavailable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let client = Client::new(Endpoint::Tcp(address));
    assert!(matches!(
        client.list_devices().await,
        Err(Error::Unavailable(_))
    ));
}
