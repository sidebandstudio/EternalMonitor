//! Answers iPads while no pipeline generation streams.
//!
//! The stream port used to be open only inside a running pipeline, so a
//! HELLO2 sent while the stream waited to restart, after Stop, or after the
//! storm brake gave up went unanswered, and the iPad said the PC didn't
//! answer. Between generations the supervisor now runs this responder on
//! the same port and the USB tunnels. It answers each HELLO2 through the
//! session, which says why nothing streams ([`Session::set_not_streaming`])
//! after the same version, update and pairing checks as always. It answers
//! an IDENTIFY through the session too, so an iPad that finds the PC at a
//! new address can check it is the saved PC while nothing streams. It never
//! starts a session or a stream, and lets go of the port before the
//! supervisor checks it for the next generation.
//!
//! While the PC restarts its stream, a session kept across the restart
//! stays alive here: its iPad gets heartbeats as from a running stream, and
//! its reports and pings are heard as there (see
//! [`Session::restart_tick`]). It used to lose the signal after three
//! seconds, while a restart can wait up to half a minute.
//!
//! [`Session::restart_tick`]: super::session::Session::restart_tick
//!
//! [`Session::set_not_streaming`]: super::session::Session::set_not_streaming

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{parse_control, ControlMessage};
use eternal_wire::v2::{classify, Classified};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::link::{LinkId, PeerId};
use super::usb::{LinkEvent, TransportLinks};
use crate::control::SharedControl;

/// How often the responder tries again to bind a port it couldn't: the
/// generation that just ended may hold it a moment longer.
const BIND_RETRY: Duration = Duration::from_millis(250);
/// How often the responder looks for a goodbye left for a stopped session.
const GOODBYE_CHECK: Duration = Duration::from_millis(250);
/// How often the responder checks the chosen network's address, so it
/// follows the network as the stream does, a little sooner.
const NETWORK_CHECK: Duration = Duration::from_secs(3);
/// How long [`Standby::stop`] waits for the port to close.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The running responder. [`Standby::stop`] closes the port.
pub struct Standby {
    stop: oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Standby {
    /// Starts answering on `listen_port`, on its own thread.
    pub fn start(listen_port: u16, shared: SharedControl) -> Option<Self> {
        let (stop, stopped) = oneshot::channel();
        std::thread::Builder::new()
            .name("standby".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run(listen_port, shared, stopped)),
                    Err(error) => warn!(%error, "Could not answer iPads while the stream is off"),
                }
            })
            .inspect_err(|error| warn!(%error, "Could not answer iPads while the stream is off"))
            .ok()
            .map(|thread| Self { stop, thread })
    }

    /// Stops answering, and returns once the port and the USB tunnels are
    /// free for the next generation.
    pub fn stop(self) {
        let _ = self.stop.send(());
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !self.thread.is_finished() {
            if Instant::now() >= deadline {
                warn!("The standby responder didn't let go of the stream port in time");
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.thread.join();
    }
}

async fn run(listen_port: u16, shared: SharedControl, mut stopped: oneshot::Receiver<()>) {
    loop {
        let (mut links, bound) = tokio::select! {
            opened = open(listen_port, &shared) => opened,
            _ = &mut stopped => return,
        };
        debug!(%bound, "Answering iPads while no stream runs");
        if !serve(&mut links, bound, &shared, &mut stopped).await {
            super::park_stream_socket(&shared, links.into_udp_socket());
            debug!("Stopped answering iPads while no stream runs");
            return;
        }
        // The chosen network went away, came back or changed its address:
        // listen where iPads can reach the PC now, as the stream would.
        info!(was = %bound, "The chosen network changed: answering iPads on its new address");
    }
}

/// Answers until the supervisor stops the responder (false), or until the
/// chosen network's address is no longer `bound` (true).
async fn serve(
    links: &mut TransportLinks,
    bound: SocketAddr,
    shared: &SharedControl,
    stopped: &mut oneshot::Receiver<()>,
) -> bool {
    let mut goodbyes = tokio::time::interval(GOODBYE_CHECK);
    let mut heartbeat = tokio::time::interval(super::session::HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut network_check = tokio::time::interval(NETWORK_CHECK);
    network_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = &mut *stopped => return false,
            _ = network_check.tick() => {
                // Every network listens on 0.0.0.0, so a change to or from
                // it shows here too.
                let listen_on = shared.listen_on.lock().clone();
                if crate::network::listen_ip_now(&listen_on) != bound.ip() {
                    return true;
                }
            }
            _ = heartbeat.tick() => {
                let config = config(shared, bound);
                let actions = shared.session.lock().restart_tick(&config, super::now());
                carry_out(links, shared, actions).await;
            }
            // A Stop during a restart pause ends a session no transport
            // can tell: say it here.
            _ = goodbyes.tick() => {
                let goodbye = shared.session.lock().take_goodbye();
                if let Some(goodbye) = goodbye {
                    super::say_goodbye(links, goodbye).await;
                }
            }
            event = links.receive() => match event {
                Ok(LinkEvent::Datagram(peer, datagram)) => {
                    answer(links, shared, bound, peer, &datagram).await;
                }
                // A tunnel that closes now is gone for the next generation
                // too; a session it carried ends on its liveness there.
                Ok(LinkEvent::Closed(_)) => {}
                Err(error) => {
                    debug!(%error, "Standby receive failed");
                    tokio::time::sleep(BIND_RETRY).await;
                }
            },
        }
    }
}

/// Takes the stream port from the generation that just ended, or binds it
/// as the transport does, retrying while that generation still holds it.
/// Returns the links and the address they listen on.
async fn open(listen_port: u16, shared: &SharedControl) -> (TransportLinks, SocketAddr) {
    let mut logged = false;
    loop {
        let listen_on = shared.listen_on.lock().clone();
        let ip = crate::network::listen_ip_now(&listen_on);
        let bind_addr = SocketAddr::from((ip, listen_port));
        let socket = match super::take_stream_socket(shared, bind_addr) {
            Some(socket) => Ok(socket),
            None => super::bind_stream_socket(bind_addr),
        };
        match socket.and_then(UdpSocket::from_std) {
            Ok(socket) => {
                if logged {
                    info!(%bind_addr, "Answering iPads while the stream is off");
                }
                if ip.is_unspecified() {
                    // Lists the home network addresses before anyone asks
                    // for them, as the stream does.
                    crate::network::home_addresses();
                }
                // The Address row and QR code follow, as for the stream.
                let shown = crate::gui::shown_address(ip, listen_port);
                crate::stats::PIPELINE_STATS.lock().set_listen_addr(shown);
                let links = TransportLinks::new(socket, Arc::clone(&shared.force_next_idr));
                return (links, bind_addr);
            }
            Err(error) if !logged => {
                logged = true;
                warn!(%bind_addr, %error, "Can't answer iPads while the stream is off yet");
            }
            Err(_) => {}
        }
        tokio::time::sleep(BIND_RETRY).await;
    }
}

fn config(shared: &SharedControl, bound: SocketAddr) -> super::SharedConfigSource<'_> {
    super::SharedConfigSource {
        shared,
        stream_epoch: 0,
        pen_available: false,
        listen: bound,
    }
}

/// Sends what the session decided while no stream runs: heartbeats and
/// pongs to the kept session, its goodbye, and the summary of a session
/// that ended.
async fn carry_out(
    links: &TransportLinks,
    shared: &SharedControl,
    actions: super::session::Actions,
) {
    if let Some(end) = &actions.ended {
        crate::diagnostics::log_session_end(end, shared);
    }
    for (destination, datagram) in &actions.replies {
        if let Err(error) = links.send_to(datagram, *destination).await {
            debug!(peer = %destination, %error, "Could not reach the iPad while the stream restarts");
        }
    }
    if let Some(goodbye) = actions.goodbye {
        super::say_goodbye(links, goodbye).await;
    }
}

/// Answers a HELLO2 or an IDENTIFY, and hears the kept session's reports,
/// pings, keyframe requests and goodbye, so it stays alive while the PC
/// restarts (see [`super::session::Session::restart_tick`]). Input and
/// repair requests wait for the next generation.
async fn answer(
    links: &TransportLinks,
    shared: &SharedControl,
    bound: SocketAddr,
    peer: PeerId,
    datagram: &[u8],
) {
    match classify(datagram) {
        Classified::Control(_) => {}
        Classified::LegacyHello => {
            shared.session.lock().note_legacy_hello(peer);
            return;
        }
        _ => return,
    }
    let Ok((header, message)) = parse_control(datagram) else {
        return;
    };
    let config = config(shared, bound);
    if matches!(
        message,
        ControlMessage::ReceiverReport(_)
            | ControlMessage::Ping(_)
            | ControlMessage::KeyframeRequest(_)
            | ControlMessage::Bye(_)
    ) {
        let actions = {
            let mut session = shared.session.lock();
            if session.not_streaming() != Some(super::session::NotStreaming::Restarting) {
                return;
            }
            // Only the kept session's own messages get anywhere: the
            // session checks their id and source, as the stream does.
            session.handle_datagram(
                peer,
                &header,
                message,
                datagram.len(),
                &config,
                super::now(),
            )
        };
        // A keyframe request asks for an IDR, which the next generation
        // starts with anyway.
        carry_out(links, shared, actions).await;
        return;
    }
    if !matches!(
        message,
        ControlMessage::Hello2(_) | ControlMessage::Identify(_)
    ) {
        return;
    }
    let actions = {
        let mut session = shared.session.lock();
        // The supervisor clears it only after stopping this responder;
        // nothing answered here starts a session.
        if session.not_streaming().is_none() {
            return;
        }
        session.handle_datagram(
            peer,
            &header,
            message,
            datagram.len(),
            &config,
            super::now(),
        )
    };
    if let Some(end) = &actions.ended {
        crate::diagnostics::log_session_end(end, shared);
    }
    for (destination, reply) in &actions.replies {
        // Over Wi-Fi an answer is never larger than the request it answers.
        // iPads that understand these answers pad their HELLO2 so every
        // answer fits (`STANDBY_HELLO2_LEN`); an older one gets the short
        // refusal, or today's silence. An IDENTIFY is as long as its answer.
        // Answers over USB stay whole.
        if destination.link == LinkId::Udp && reply.len() > datagram.len() {
            debug!(
                peer = %destination,
                reply = reply.len(),
                request = datagram.len(),
                "Left a request unanswered: the answer is larger than it"
            );
            continue;
        }
        if let Err(error) = links.send_to(reply, *destination).await {
            debug!(peer = %destination, %error, "Could not answer an iPad while the stream is off");
        }
    }
}
