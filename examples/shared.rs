use std::sync::Arc;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use bevy_spicy_networking::{ClientMessage, NetworkMessage, ServerMessage};
use bevy_spicy_networking_tokio_quinn::{TokioQuinnServerProvider, TokioQuinnClientProvider, ServerConfig, ClientConfig};

/////////////////////////////////////////////////////////////////////
// In this example the client sends `UserChatMessage`s to the server,
// the server then broadcasts to all connected clients.
//
// We use two different types here, because only the server should
// decide the identity of a given connection and thus also sends a
// name.
//
// You can have a single message be sent both ways, it simply needs
// to implement both `ClientMessage` and `ServerMessage`.
/////////////////////////////////////////////////////////////////////

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserChatMessage {
    pub message: String,
}

#[typetag::serde]
impl NetworkMessage for UserChatMessage {}

impl ServerMessage for UserChatMessage {
    const NAME: &'static str = "example:UserChatMessage";
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NewChatMessage {
    pub name: String,
    pub message: String,
}

#[typetag::serde]
impl NetworkMessage for NewChatMessage {}

impl ClientMessage for NewChatMessage {
    const NAME: &'static str = "example:NewChatMessage";
}

#[allow(unused)]
pub fn client_register_network_messages(app: &mut App) {
    use bevy_spicy_networking::AppNetworkClientMessage;

    // The client registers messages that arrives from the server, so that
    // it is prepared to handle them. Otherwise, an error occurs.
    app.listen_for_client_message::<NewChatMessage, TokioQuinnClientProvider>();
}

#[allow(unused)]
pub fn server_register_network_messages(app: &mut App) {
    use bevy_spicy_networking::AppNetworkServerMessage;

    // The server registers messages that arrives from a client, so that
    // it is prepared to handle them. Otherwise, an error occurs.
    app.listen_for_server_message::<UserChatMessage, TokioQuinnServerProvider>();
}

// Implementation of `ServerCertVerifier` that verifies everything as trustworthy.
pub struct SkipServerVerification;

impl SkipServerVerification {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

pub fn configure_client() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

    ClientConfig::new(Arc::new(crypto))
}

/// Returns default server configuration along with its certificate.
#[allow(clippy::field_reassign_with_default)] // https://github.com/rust-lang/rust-clippy/issues/6527
pub fn configure_server() -> (ServerConfig, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key).unwrap();
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .max_concurrent_uni_streams(0_u8.into());

    (server_config, cert_der)
}