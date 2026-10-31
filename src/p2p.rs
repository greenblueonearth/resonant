use crate::wallet::Wallet;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snow::{params::NoiseParams, Builder, TransportState};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_FRAME_SIZE: usize = 64 * 1024;
const MAX_PEERS: usize = 64;
const MAX_QUEUED_MESSAGES: usize = 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

#[derive(Clone, Debug, PartialEq)]
pub struct ReceivedMessage {
    pub from: SocketAddr,
    pub peer_public_key: String,
    pub payload: Value,
}

pub struct P2pNode {
    local_addr: SocketAddr,
    advertised_addr: SocketAddr,
    identity: Wallet,
    trusted_peers: BTreeSet<String>,
    peers: Arc<Mutex<BTreeSet<SocketAddr>>>,
    messages: Receiver<ReceivedMessage>,
    stop: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
}

impl P2pNode {
    pub fn bind(
        address: SocketAddr,
        identity: Wallet,
        trusted_peers: BTreeSet<String>,
    ) -> Result<Self, String> {
        Self::bind_with_advertise(address, address, identity, trusted_peers)
    }

    pub fn bind_with_advertise(
        address: SocketAddr,
        advertised_addr: SocketAddr,
        identity: Wallet,
        trusted_peers: BTreeSet<String>,
    ) -> Result<Self, String> {
        validate_identity(&identity)?;
        for public_key in &trusted_peers {
            validate_public_key(public_key)?;
        }
        if trusted_peers.contains(&identity.public_key) {
            return Err("local peer identity must not be in the trusted peer list".to_string());
        }

        let listener = TcpListener::bind(address)
            .map_err(|error| format!("could not listen on {address}: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("could not configure listener: {error}"))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("could not read listener address: {error}"))?;
        let advertised_addr = SocketAddr::new(
            advertised_addr.ip(),
            if advertised_addr.port() == 0 {
                local_addr.port()
            } else {
                advertised_addr.port()
            },
        );
        if advertised_addr.ip().is_unspecified() || advertised_addr.port() == 0 {
            return Err(
                "wildcard listen addresses require a reachable --advertise address".to_string(),
            );
        }
        let peers = Arc::new(Mutex::new(BTreeSet::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (message_sender, messages) = mpsc::sync_channel(MAX_QUEUED_MESSAGES);
        let listener_peers = Arc::clone(&peers);
        let listener_stop = Arc::clone(&stop);
        let listener_identity = identity.clone();
        let listener_trusted_peers = trusted_peers.clone();

        let listener_thread = thread::Builder::new()
            .name("resonant-p2p-listener".to_string())
            .spawn(move || {
                serve(
                    listener,
                    advertised_addr,
                    listener_identity,
                    listener_trusted_peers,
                    listener_peers,
                    message_sender,
                    listener_stop,
                )
            })
            .map_err(|error| format!("could not start P2P listener: {error}"))?;

        Ok(Self {
            local_addr,
            advertised_addr,
            identity,
            trusted_peers,
            peers,
            messages,
            stop,
            listener_thread: Some(listener_thread),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn advertised_addr(&self) -> SocketAddr {
        self.advertised_addr
    }

    pub fn public_key(&self) -> &str {
        &self.identity.public_key
    }

    pub fn peers(&self) -> Result<Vec<SocketAddr>, String> {
        self.peers
            .lock()
            .map(|peers| peers.iter().copied().collect())
            .map_err(|_| "P2P peer list is unavailable".to_string())
    }

    pub fn discover(&self, seeds: &[SocketAddr]) -> Result<Vec<SocketAddr>, String> {
        let mut discovered = BTreeSet::new();
        let mut queued: BTreeSet<_> = seeds
            .iter()
            .copied()
            .filter(|address| *address != self.advertised_addr)
            .collect();

        while let Some(address) = queued.pop_first() {
            if discovered.contains(&address) {
                continue;
            }
            if discovered.len() >= MAX_PEERS {
                return Err(format!(
                    "peer discovery exceeded the {MAX_PEERS}-peer limit"
                ));
            }
            let response = exchange(
                address,
                &Request::Handshake {
                    address: self.advertised_addr,
                },
                &self.identity,
                &self.trusted_peers,
            )?;
            let Response::Peers { addresses } = response else {
                return Err(format!(
                    "peer {address} returned an invalid handshake response"
                ));
            };

            self.remember_peer(address)?;
            discovered.insert(address);
            for peer in addresses {
                if peer != self.advertised_addr && !discovered.contains(&peer) {
                    queued.insert(peer);
                }
            }
        }

        Ok(discovered.into_iter().collect())
    }

    pub fn send(&self, address: SocketAddr, payload: Value) -> Result<(), String> {
        if address == self.advertised_addr {
            return Err("cannot send a P2P message to this node".to_string());
        }
        match exchange(
            address,
            &Request::Publish {
                from: self.advertised_addr,
                payload,
            },
            &self.identity,
            &self.trusted_peers,
        )? {
            Response::Accepted => self.remember_peer(address),
            Response::Error { message } => {
                Err(format!("peer {address} rejected message: {message}"))
            }
            Response::Peers { .. } => Err(format!(
                "peer {address} returned an invalid message response"
            )),
        }
    }

    pub fn broadcast(&self, payload: Value) -> Result<(), String> {
        let peers = self.peers()?;
        let mut failures = Vec::new();
        for peer in peers {
            if let Err(error) = self.send(peer, payload.clone()) {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<ReceivedMessage, String> {
        match self.messages.recv_timeout(timeout) {
            Ok(message) => Ok(message),
            Err(RecvTimeoutError::Timeout) => {
                Err("timed out waiting for a P2P message".to_string())
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err("P2P message receiver has stopped".to_string())
            }
        }
    }

    fn remember_peer(&self, address: SocketAddr) -> Result<(), String> {
        remember_peer(&self.peers, self.advertised_addr, address)
    }
}

impl Drop for P2pNode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(listener_thread) = self.listener_thread.take() {
            let _ = listener_thread.join();
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Request {
    Handshake { address: SocketAddr },
    Publish { from: SocketAddr, payload: Value },
}

#[derive(Serialize, Deserialize)]
enum Response {
    Peers { addresses: Vec<SocketAddr> },
    Accepted,
    Error { message: String },
}

fn serve(
    listener: TcpListener,
    advertised_addr: SocketAddr,
    identity: Wallet,
    trusted_peers: BTreeSet<String>,
    peers: Arc<Mutex<BTreeSet<SocketAddr>>>,
    messages: SyncSender<ReceivedMessage>,
    stop: Arc<AtomicBool>,
) {
    let mut handlers = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        handlers.retain(|handler: &JoinHandle<()>| !handler.is_finished());
        match listener.accept() {
            Ok((stream, _)) if handlers.len() < MAX_PEERS => {
                let identity = identity.clone();
                let trusted_peers = trusted_peers.clone();
                let peers = Arc::clone(&peers);
                let messages = messages.clone();
                match thread::Builder::new()
                    .name("resonant-p2p-connection".to_string())
                    .spawn(move || {
                        serve_connection(
                            stream,
                            advertised_addr,
                            identity,
                            trusted_peers,
                            peers,
                            messages,
                        )
                    }) {
                    Ok(handler) => handlers.push(handler),
                    Err(error) => eprintln!("could not start P2P connection handler: {error}"),
                }
            }
            Ok((_stream, _)) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => thread::sleep(ACCEPT_POLL_INTERVAL),
        }
    }
    for handler in handlers {
        let _ = handler.join();
    }
}

fn serve_connection(
    mut stream: TcpStream,
    advertised_addr: SocketAddr,
    identity: Wallet,
    trusted_peers: BTreeSet<String>,
    peers: Arc<Mutex<BTreeSet<SocketAddr>>>,
    messages: SyncSender<ReceivedMessage>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Ok((mut transport, peer_public_key)) =
        authenticated_transport(&mut stream, false, &identity, &trusted_peers)
    else {
        return;
    };
    let response = process_request(
        &mut stream,
        &mut transport,
        peer_public_key,
        advertised_addr,
        &peers,
        &messages,
    );
    let _ = write_encrypted_frame(&mut stream, &mut transport, &response);
}

fn process_request(
    stream: &mut TcpStream,
    transport: &mut TransportState,
    peer_public_key: String,
    advertised_addr: SocketAddr,
    peers: &Mutex<BTreeSet<SocketAddr>>,
    messages: &SyncSender<ReceivedMessage>,
) -> Response {
    match read_encrypted_frame::<Request>(stream, transport) {
        Ok(Request::Handshake { address }) => {
            if let Err(error) = remember_peer(peers, advertised_addr, address) {
                Response::Error { message: error }
            } else {
                match peers.lock() {
                    Ok(peers) => Response::Peers {
                        addresses: peers.iter().copied().collect(),
                    },
                    Err(_) => Response::Error {
                        message: "P2P peer list is unavailable".to_string(),
                    },
                }
            }
        }
        Ok(Request::Publish { from, payload }) => match remember_peer(peers, advertised_addr, from)
        {
            Ok(()) => match messages.try_send(ReceivedMessage {
                from,
                peer_public_key,
                payload,
            }) {
                Ok(()) => Response::Accepted,
                Err(TrySendError::Full(_)) => Response::Error {
                    message: "inbound message queue is full".to_string(),
                },
                Err(TrySendError::Disconnected(_)) => Response::Error {
                    message: "message receiver is unavailable".to_string(),
                },
            },
            Err(error) => Response::Error { message: error },
        },
        Err(_) => Response::Error {
            message: "invalid encrypted P2P request".to_string(),
        },
    }
}

fn remember_peer(
    peers: &Mutex<BTreeSet<SocketAddr>>,
    local_addr: SocketAddr,
    address: SocketAddr,
) -> Result<(), String> {
    if address == local_addr {
        return Ok(());
    }
    if address.ip().is_unspecified() || address.port() == 0 {
        return Err("peer advertised an unusable address".to_string());
    }
    let mut peers = peers
        .lock()
        .map_err(|_| "P2P peer list is unavailable".to_string())?;
    if peers.len() >= MAX_PEERS && !peers.contains(&address) {
        return Err(format!("peer list reached the {MAX_PEERS}-peer limit"));
    }
    peers.insert(address);
    Ok(())
}

fn exchange(
    address: SocketAddr,
    request: &Request,
    identity: &Wallet,
    trusted_peers: &BTreeSet<String>,
) -> Result<Response, String> {
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)
        .map_err(|error| format!("could not connect to peer {address}: {error}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("could not configure connection to {address}: {error}"))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("could not configure connection to {address}: {error}"))?;
    let (mut transport, _) = authenticated_transport(&mut stream, true, identity, trusted_peers)?;
    write_encrypted_frame(&mut stream, &mut transport, request)
        .map_err(|error| format!("could not send request to peer {address}: {error}"))?;
    read_encrypted_frame(&mut stream, &mut transport)
        .map_err(|error| format!("could not read response from peer {address}: {error}"))
}

fn write_bytes(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    if payload.is_empty() || payload.len() > MAX_FRAME_SIZE {
        return Err(format!(
            "P2P frame length must be between 1 and {MAX_FRAME_SIZE} bytes"
        ));
    }
    let length = u32::try_from(payload.len()).map_err(|_| "P2P frame is too large".to_string())?;
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|()| stream.write_all(payload))
        .map_err(|error| format!("could not write P2P frame: {error}"))
}

fn read_bytes(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut length_bytes = [0; 4];
    stream
        .read_exact(&mut length_bytes)
        .map_err(|error| format!("could not read P2P frame length: {error}"))?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_FRAME_SIZE {
        return Err(format!(
            "P2P frame length must be between 1 and {MAX_FRAME_SIZE} bytes"
        ));
    }
    let mut payload = vec![0; length];
    stream
        .read_exact(&mut payload)
        .map_err(|error| format!("could not read P2P frame: {error}"))?;
    Ok(payload)
}

fn write_encrypted_frame<T: Serialize>(
    stream: &mut TcpStream,
    transport: &mut TransportState,
    value: &T,
) -> Result<(), String> {
    let plaintext = serde_json::to_vec(value)
        .map_err(|error| format!("could not encode P2P frame: {error}"))?;
    let mut ciphertext = vec![0; MAX_FRAME_SIZE];
    let length = transport
        .write_message(&plaintext, &mut ciphertext)
        .map_err(|error| format!("could not encrypt P2P frame: {error}"))?;
    write_bytes(stream, &ciphertext[..length])
}

fn read_encrypted_frame<T: DeserializeOwned>(
    stream: &mut TcpStream,
    transport: &mut TransportState,
) -> Result<T, String> {
    let ciphertext = read_bytes(stream)?;
    let mut plaintext = vec![0; MAX_FRAME_SIZE];
    let length = transport
        .read_message(&ciphertext, &mut plaintext)
        .map_err(|error| format!("could not authenticate/decrypt P2P frame: {error}"))?;
    serde_json::from_slice(&plaintext[..length])
        .map_err(|error| format!("invalid encrypted P2P frame: {error}"))
}

fn authenticated_transport(
    stream: &mut TcpStream,
    initiator: bool,
    identity: &Wallet,
    trusted_peers: &BTreeSet<String>,
) -> Result<(TransportState, String), String> {
    let params: NoiseParams = NOISE_PATTERN
        .parse()
        .map_err(|error| format!("invalid Noise protocol configuration: {error}"))?;
    let builder = Builder::new(params);
    let keypair = builder
        .generate_keypair()
        .map_err(|error| format!("could not generate Noise session key: {error}"))?;
    let builder = builder
        .local_private_key(&keypair.private)
        .map_err(|error| format!("could not configure Noise session key: {error}"))?;
    let mut noise = if initiator {
        builder
            .build_initiator()
            .map_err(|error| format!("could not initialize Noise initiator: {error}"))?
    } else {
        builder
            .build_responder()
            .map_err(|error| format!("could not initialize Noise responder: {error}"))?
    };
    let mut buffer = vec![0; MAX_FRAME_SIZE];

    if initiator {
        let length = noise
            .write_message(&[], &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
        write_bytes(stream, &buffer[..length])?;
        let message = read_bytes(stream)?;
        noise
            .read_message(&message, &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
        let length = noise
            .write_message(&[], &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
        write_bytes(stream, &buffer[..length])?;
    } else {
        let message = read_bytes(stream)?;
        noise
            .read_message(&message, &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
        let length = noise
            .write_message(&[], &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
        write_bytes(stream, &buffer[..length])?;
        let message = read_bytes(stream)?;
        noise
            .read_message(&message, &mut buffer)
            .map_err(|error| format!("Noise handshake failed: {error}"))?;
    }

    let handshake_hash = noise.get_handshake_hash().to_vec();
    let mut transport = noise
        .into_transport_mode()
        .map_err(|error| format!("could not establish Noise transport: {error}"))?;
    let role = if initiator { "initiator" } else { "responder" };
    let auth = PeerAuth {
        public_key: identity.public_key.clone(),
        role: role.to_string(),
        signature: identity.sign(&peer_auth_message(
            &handshake_hash,
            role,
            &identity.public_key,
        ))?,
    };

    if initiator {
        write_encrypted_frame(stream, &mut transport, &auth)?;
        let remote: PeerAuth = read_encrypted_frame(stream, &mut transport)?;
        verify_peer_auth(&remote, "responder", &handshake_hash, trusted_peers)?;
        Ok((transport, remote.public_key))
    } else {
        let remote: PeerAuth = read_encrypted_frame(stream, &mut transport)?;
        verify_peer_auth(&remote, "initiator", &handshake_hash, trusted_peers)?;
        write_encrypted_frame(stream, &mut transport, &auth)?;
        Ok((transport, remote.public_key))
    }
}

#[derive(Serialize, Deserialize)]
struct PeerAuth {
    public_key: String,
    role: String,
    signature: String,
}

fn verify_peer_auth(
    auth: &PeerAuth,
    expected_role: &str,
    handshake_hash: &[u8],
    trusted_peers: &BTreeSet<String>,
) -> Result<(), String> {
    validate_public_key(&auth.public_key)?;
    if auth.role != expected_role || !trusted_peers.contains(&auth.public_key) {
        return Err("peer identity is not trusted".to_string());
    }
    if !Wallet::verify(
        &auth.public_key,
        &peer_auth_message(handshake_hash, expected_role, &auth.public_key),
        &auth.signature,
    ) {
        return Err("peer identity signature is invalid".to_string());
    }
    Ok(())
}

fn validate_identity(identity: &Wallet) -> Result<(), String> {
    if Wallet::address_for_public_key(&identity.public_key)? != identity.address
        || !Wallet::verify(
            &identity.public_key,
            b"resonant/p2p-identity-check/v1",
            &identity.sign(b"resonant/p2p-identity-check/v1")?,
        )
    {
        return Err("invalid P2P identity key".to_string());
    }
    Ok(())
}

fn validate_public_key(public_key: &str) -> Result<(), String> {
    let decoded = hex::decode(public_key)
        .map_err(|error| format!("invalid trusted peer public key: {error}"))?;
    if decoded.len() != 32 || decoded.iter().all(|byte| *byte == 0) {
        return Err("invalid trusted peer public key".to_string());
    }
    if hex::encode(decoded) != public_key {
        return Err("trusted peer public key must use lowercase hex".to_string());
    }
    Wallet::address_for_public_key(public_key)
        .map(|_| ())
        .map_err(|error| format!("invalid trusted peer public key: {error}"))
}

fn peer_auth_message(handshake_hash: &[u8], role: &str, public_key: &str) -> Vec<u8> {
    serde_json::to_vec(&(
        "resonant/p2p-peer-auth/v1",
        hex::encode(handshake_hash),
        role,
        public_key,
    ))
    .expect("peer authentication message serialization is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn auth(wallet: &Wallet, role: &str, handshake_hash: &[u8]) -> PeerAuth {
        PeerAuth {
            public_key: wallet.public_key.clone(),
            role: role.to_string(),
            signature: wallet
                .sign(&peer_auth_message(handshake_hash, role, &wallet.public_key))
                .unwrap(),
        }
    }

    fn local_node(identity: Wallet, trusted_peers: &[&Wallet]) -> P2pNode {
        P2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            identity,
            trusted_peers
                .iter()
                .map(|wallet| wallet.public_key.clone())
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn discovers_peers_through_a_bootstrap_node() {
        let first_identity = Wallet::new("first");
        let second_identity = Wallet::new("second");
        let third_identity = Wallet::new("third");
        let first = local_node(first_identity.clone(), &[&second_identity, &third_identity]);
        let second = local_node(second_identity.clone(), &[&first_identity, &third_identity]);
        let third = local_node(third_identity.clone(), &[&first_identity, &second_identity]);

        second.discover(&[first.local_addr()]).unwrap();
        let discovered = third.discover(&[first.local_addr()]).unwrap();

        assert!(discovered.contains(&first.local_addr()));
        assert!(discovered.contains(&second.local_addr()));
        assert!(third.peers().unwrap().contains(&second.local_addr()));
    }

    #[test]
    fn sends_encrypted_messages_between_authenticated_peers() {
        let sender_identity = Wallet::new("sender");
        let receiver_identity = Wallet::new("receiver");
        let sender = local_node(sender_identity.clone(), &[&receiver_identity]);
        let receiver = local_node(receiver_identity.clone(), &[&sender_identity]);
        sender.discover(&[receiver.local_addr()]).unwrap();
        let payload = json!({"type": "vote", "height": 3});

        sender.send(receiver.local_addr(), payload.clone()).unwrap();

        let message = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(message.from, sender.local_addr());
        assert_eq!(message.peer_public_key, sender_identity.public_key);
        assert_eq!(message.payload, payload);
    }

    #[test]
    fn rejects_untrusted_peer_identities() {
        let sender_identity = Wallet::new("sender");
        let receiver_identity = Wallet::new("receiver");
        let attacker_identity = Wallet::new("attacker");
        let sender = local_node(sender_identity, &[&attacker_identity]);
        let receiver = local_node(receiver_identity, &[]);

        assert!(sender
            .send(receiver.local_addr(), json!({"type": "vote"}))
            .is_err());
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn rejects_invalid_peer_key_encodings() {
        assert!(validate_public_key("not-hex").is_err());
        assert!(validate_public_key("AA".repeat(32).as_str()).is_err());
        assert!(validate_public_key(&"00".repeat(32)).is_err());
    }

    #[test]
    fn validates_a_well_formed_local_identity() {
        assert!(validate_identity(&Wallet::new("identity")).is_ok());
    }

    #[test]
    fn rejects_identity_with_mismatched_address() {
        let mut identity = Wallet::new("identity");
        identity.address.push('x');

        assert!(validate_identity(&identity).is_err());
    }

    #[test]
    fn rejects_identity_with_malformed_secret() {
        let mut identity = Wallet::new("identity");
        identity.secret = "invalid".to_string();

        assert!(validate_identity(&identity).is_err());
    }

    #[test]
    fn accepts_canonical_public_key() {
        let identity = Wallet::new("identity");

        assert!(validate_public_key(&identity.public_key).is_ok());
    }

    #[test]
    fn rejects_public_key_with_wrong_length() {
        assert!(validate_public_key("ab".repeat(31).as_str()).is_err());
    }

    #[test]
    fn rejects_zero_public_key() {
        assert!(validate_public_key(&"00".repeat(32)).is_err());
    }

    #[test]
    fn verifies_signature_from_allowlisted_peer() {
        let peer = Wallet::new("peer");
        let trusted = BTreeSet::from([peer.public_key.clone()]);
        let handshake_hash = b"handshake";

        assert!(verify_peer_auth(
            &auth(&peer, "initiator", handshake_hash),
            "initiator",
            handshake_hash,
            &trusted
        )
        .is_ok());
    }

    #[test]
    fn rejects_peer_auth_with_wrong_role() {
        let peer = Wallet::new("peer");
        let trusted = BTreeSet::from([peer.public_key.clone()]);
        let handshake_hash = b"handshake";

        assert!(verify_peer_auth(
            &auth(&peer, "responder", handshake_hash),
            "initiator",
            handshake_hash,
            &trusted
        )
        .is_err());
    }

    #[test]
    fn rejects_valid_identity_not_in_allowlist() {
        let peer = Wallet::new("peer");
        let trusted = BTreeSet::new();

        assert!(verify_peer_auth(
            &auth(&peer, "initiator", b"handshake"),
            "initiator",
            b"handshake",
            &trusted
        )
        .is_err());
    }

    #[test]
    fn rejects_peer_auth_with_invalid_signature() {
        let peer = Wallet::new("peer");
        let trusted = BTreeSet::from([peer.public_key.clone()]);
        let mut forged = auth(&peer, "initiator", b"handshake");
        forged.signature.replace_range(0..2, "00");

        assert!(verify_peer_auth(&forged, "initiator", b"handshake", &trusted).is_err());
    }

    #[test]
    fn binds_peer_auth_signature_to_handshake_hash() {
        let peer = Wallet::new("peer");
        let trusted = BTreeSet::from([peer.public_key.clone()]);

        assert!(verify_peer_auth(
            &auth(&peer, "initiator", b"original"),
            "initiator",
            b"modified",
            &trusted
        )
        .is_err());
    }

    #[test]
    fn ignores_local_address_when_remembering_peer() {
        let local: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let peers = Mutex::new(BTreeSet::new());

        remember_peer(&peers, local, local).unwrap();

        assert!(peers.lock().unwrap().is_empty());
    }

    #[test]
    fn rejects_unspecified_peer_address() {
        let peers = Mutex::new(BTreeSet::new());
        let local = "127.0.0.1:1000".parse().unwrap();

        assert!(remember_peer(&peers, local, "0.0.0.0:1001".parse().unwrap()).is_err());
    }

    #[test]
    fn rejects_peer_address_with_zero_port() {
        let peers = Mutex::new(BTreeSet::new());
        let local = "127.0.0.1:1000".parse().unwrap();

        assert!(remember_peer(&peers, local, "127.0.0.1:0".parse().unwrap()).is_err());
    }

    #[test]
    fn remembering_same_peer_is_idempotent() {
        let peers = Mutex::new(BTreeSet::new());
        let local = "127.0.0.1:1000".parse().unwrap();
        let peer = "127.0.0.1:1001".parse().unwrap();

        remember_peer(&peers, local, peer).unwrap();
        remember_peer(&peers, local, peer).unwrap();

        assert_eq!(peers.lock().unwrap().len(), 1);
    }

    #[test]
    fn enforces_peer_limit_without_evicting_existing_peers() {
        let peers = Mutex::new(
            (1..=MAX_PEERS)
                .map(|port| SocketAddr::from(([127, 0, 0, 1], port as u16)))
                .collect(),
        );
        let local = "127.0.0.1:50000".parse().unwrap();
        let existing = "127.0.0.1:1".parse().unwrap();
        let new_peer = "127.0.0.1:50001".parse().unwrap();

        assert!(remember_peer(&peers, local, existing).is_ok());
        assert!(remember_peer(&peers, local, new_peer).is_err());
        assert_eq!(peers.lock().unwrap().len(), MAX_PEERS);
    }

    #[test]
    fn rejects_local_identity_in_trusted_peer_list() {
        let identity = Wallet::new("identity");

        assert!(P2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            identity.clone(),
            BTreeSet::from([identity.public_key])
        )
        .is_err());
    }

    #[test]
    fn rejects_malformed_trusted_peer_key_at_bind_time() {
        assert!(P2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            Wallet::new("identity"),
            BTreeSet::from(["not-a-public-key".to_string()])
        )
        .is_err());
    }

    #[test]
    fn rejects_wildcard_advertised_address() {
        assert!(P2pNode::bind_with_advertise(
            "127.0.0.1:0".parse().unwrap(),
            "0.0.0.0:0".parse().unwrap(),
            Wallet::new("identity"),
            BTreeSet::new()
        )
        .is_err());
    }

    #[test]
    fn uses_bound_port_when_advertised_port_is_zero() {
        let node = P2pNode::bind_with_advertise(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
            Wallet::new("identity"),
            BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(node.advertised_addr().port(), node.local_addr().port());
    }

    #[test]
    fn rejects_sending_message_to_self() {
        let identity = Wallet::new("identity");
        let node = local_node(identity, &[]);

        assert!(node
            .send(node.advertised_addr(), json!({"type": "ping"}))
            .is_err());
    }

    #[test]
    fn discovery_skips_the_local_advertised_address() {
        let identity = Wallet::new("identity");
        let node = local_node(identity, &[]);

        assert!(node.discover(&[node.advertised_addr()]).unwrap().is_empty());
    }
}
