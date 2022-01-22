use std::{net::SocketAddr, io::ErrorKind, sync::Arc};

use bevy::{prelude::{error, trace, debug}, utils::HashMap};
use bevy_spicy_networking::{async_trait, server::NetworkServerProvider, client::NetworkClientProvider, NetworkPacket, ClientNetworkEvent, error::NetworkError};
use futures::future::{join_all, select_all, select, Either, join3};
use quinn::{Endpoint, NewConnection, Connecting, IncomingBiStreams, SendStream, RecvStream, Connection, ConnectionError};
use tokio::{net::UdpSocket, io::{AsyncReadExt, AsyncWriteExt}, runtime::Runtime, sync::mpsc::{UnboundedSender, UnboundedReceiver, unbounded_channel}};
use futures_util::stream::StreamExt;

pub use quinn::{ServerConfig, ClientConfig};

#[derive(Default)]
pub struct TokioQuinnServerProvider;

unsafe impl Send for TokioQuinnServerProvider{}
unsafe impl Sync for TokioQuinnServerProvider{}

#[async_trait]
impl NetworkServerProvider for TokioQuinnServerProvider{
    
    type NetworkSettings = ServerNetworkSettings;

    type Socket = (Connection, SendStream, RecvStream);

    type ReadHalf = (Arc<Connection>, RecvStream);

    type WriteHalf = (Arc<Connection>, SendStream);

    async fn accept_loop(network_settings: Self::NetworkSettings, new_connections: UnboundedSender<Self::Socket>, errors: UnboundedSender<NetworkError>){
        let (endpoint, mut incoming) = Endpoint::server(network_settings.config, network_settings.addr).unwrap();

        let (new_connectings_tx, mut new_connectings_rx) = unbounded_channel();
        let incoming_bi_streams: Vec<IncomingBiStreams> = Vec::default();

        let incoming_errors = errors.clone();
        let incoming_handler = Box::pin(async move {
            loop{
                if let Some(new_connection) = incoming.next().await{
                    if let Ok(_) = new_connectings_tx.send(new_connection){
                        continue;
                    }
                    error!("Couldn't send the new incoming connection to the handler!");
                    break;
                };
                if let Ok(_) = errors.send(NetworkError::Listen(tokio::io::Error::new(ErrorKind::Other, "incoming connection failed!"))){
                    error!("Couldn't listen for incoming connections!");
                    continue;
                }
            };
        });

        let (new_connections_tx, mut new_connections_rx) = unbounded_channel();
        let connecting_handler = Box::pin(async move{
            let mut connecting = Vec::new();
            // Rebind here to prevent drop in loop
            loop{
                if connecting.len() > 0{
                    let finished = select(
                        Box::pin(new_connectings_rx.recv()),
                        select_all(connecting.drain(..))
                    ); 

                    match finished.await{
                        Either::Left((new_connecting, still_connecting)) => {
                            if let Some(new_connecting) = new_connecting{
                                connecting = still_connecting.into_inner();
                                connecting.push(new_connecting);
                            };
                        },
                        Either::Right(((connected, index, mut still_connecting), _)) =>{
                            if let Ok(connection) = connected{
                                new_connections_tx.send(connection);
                            };
                            connecting = still_connecting
                                .drain(..)
                                .enumerate()
                                .filter_map(|(n, i)| if n == index {None} else {Some(i)})
                                .collect();
                        }
                    };
                } else {
                    if let Some(new_connecting) = new_connectings_rx.recv().await{
                        connecting.push(new_connecting);
                    }
                }
            };
        });

        let new_connection_handler = Box::pin(async move{
            let mut bi_streams = Vec::new();
            // Rebind here to prevent drop in loop
            loop{
                if bi_streams.len() > 0{
                    let finished = select(
                        Box::pin(new_connections_rx.recv()),
                        select_all(bi_streams.drain(..))
                    ); 

                    match finished.await{
                        Either::Left((new_connecting, still_connecting)) => {
                            if let Some(new_connection) = new_connecting{
                                bi_streams = still_connecting.into_inner();
                                bi_streams.push(Box::pin(new_connection_to_bi_connection(new_connection)));
                            };
                        },
                        Either::Right((((connected, streams), index, mut still_connecting), _)) =>{
                            if let Ok((send, recv)) = streams{
                                new_connections.send((connected, send, recv));
                            };
                            bi_streams = still_connecting
                                .drain(..)
                                .enumerate()
                                .filter_map(|(n, i)| if n == index {None} else {Some(i)})
                                .collect();
                        }
                    };
                } else {
                    if let Some(new_connection) = new_connections_rx.recv().await{
                        bi_streams.push(Box::pin(new_connection_to_bi_connection(new_connection)));
                    }
                }
            };
        });

        join3(
                new_connection_handler,
                connecting_handler,
                incoming_handler
        ).await;
    }

    async fn recv_loop(mut read_half: Self::ReadHalf, messages: UnboundedSender<NetworkPacket>, settings: Self::NetworkSettings){
        let (connection, mut read_half) = read_half;
        loop {
            let chunk = match read_half.read_chunk(settings.max_packet_length, false).await {
                Ok(chunk) => chunk.unwrap(),
                Err(err) => {
                    error!(
                        "Encountered error while reading chunk: {}",
                        err
                    );
                    break;
                }
            };

            let packet: NetworkPacket = match bincode::deserialize(&chunk.bytes) {
                Ok(packet) => packet,
                Err(err) => {
                    error!(
                        "Failed to decode network packet from: {}",
                        err
                    );
                    break;
                }
            };

            if let Err(_) = messages.send(packet){
                error!("Failed to send decoded message to Spicy");
                break;
            }
        }
    }

    async fn send_loop(mut write_half: Self::WriteHalf, mut messages: UnboundedReceiver<NetworkPacket>, settings: Self::NetworkSettings){
        let (connection, mut write_half) = write_half;
        while let Some(message) = messages.recv().await {
            let encoded = match bincode::serialize(&message) {
                Ok(encoded) => encoded,
                Err(err) => {
                    error!("Could not encode packet {:?}: {}", message, err);
                    continue;
                }
            };

            match write_half.write_chunk(encoded.into()).await {
                Ok(_) => (),
                Err(err) => {
                    error!("Could not send packet: {:?}: {}", message, err);
                    break;
                }
            }

            trace!("Succesfully written all!");
        }
    }
    

    fn split(combined: Self::Socket) -> (Self::ReadHalf, Self::WriteHalf){
        let (connection, write, read) = combined;
        let connection = Arc::new(connection);
        (
            (connection.clone(), read),
            (connection, write)
        )
    }
}

#[derive(Default)]
pub struct TokioQuinnClientProvider;

unsafe impl Send for TokioQuinnClientProvider{}
unsafe impl Sync for TokioQuinnClientProvider{}

#[async_trait]
impl NetworkClientProvider for TokioQuinnClientProvider{
    
    type NetworkSettings = ClientNetworkSettings;

    type Socket = (Connection, SendStream, RecvStream);

    type ReadHalf = (Arc<Connection>, RecvStream);

    type WriteHalf = (Arc<Connection>, SendStream);

    async fn connect_task(network_settings: Self::NetworkSettings, new_connections: UnboundedSender<Self::Socket>, errors: UnboundedSender<ClientNetworkEvent>){
        let endpoint = Endpoint::client(network_settings.bind_addr).unwrap();
        let connecting = endpoint.connect_with(network_settings.config, network_settings.connect_addr, &network_settings.server_name).unwrap();
        let new_connection = connecting.await.unwrap();
        let (tx, rx) = new_connection.connection.open_bi().await.unwrap();
        new_connections.send((new_connection.connection, tx, rx)).unwrap();
    }

    async fn recv_loop(mut read_half: Self::ReadHalf, messages: UnboundedSender<NetworkPacket>, settings: Self::NetworkSettings){
        let (connection, mut read_half) = read_half;
        loop {
            let chunk = match read_half.read_chunk(settings.max_packet_length, false).await {
                Ok(chunk) => chunk.unwrap(),
                Err(err) => {
                    error!(
                        "Encountered error while reading chunk: {}",
                        err
                    );
                    break;
                }
            };

            let packet: NetworkPacket = match bincode::deserialize(&chunk.bytes) {
                Ok(packet) => packet,
                Err(err) => {
                    error!(
                        "Failed to decode network packet from: {}",
                        err
                    );
                    break;
                }
            };

            if let Err(_) = messages.send(packet){
                error!("Failed to send decoded message to Spicy");
                break;
            }
        }
    }
    
    async fn send_loop(mut write_half: Self::WriteHalf, mut messages: UnboundedReceiver<NetworkPacket>, settings: Self::NetworkSettings){
        let (connection, mut write_half) = write_half;
        while let Some(message) = messages.recv().await {
            let encoded = match bincode::serialize(&message) {
                Ok(encoded) => encoded,
                Err(err) => {
                    error!("Could not encode packet {:?}: {}", message, err);
                    continue;
                }
            };

            match write_half.write_chunk(encoded.into()).await {
                Ok(_) => (),
                Err(err) => {
                    error!("Could not send packet: {:?}: {}", message, err);
                    break;
                }
            }

            trace!("Succesfully written all!");
        }
    }

    fn split(combined: Self::Socket) -> (Self::ReadHalf, Self::WriteHalf){
        let (connection, write, read) = combined;
        let connection = Arc::new(connection);
        (
            (connection.clone(), read),
            (connection, write)
        )
    }
}

#[derive(Clone, Debug)]
#[allow(missing_copy_implementations)]
/// Settings to configure the network, both client and server
pub struct ClientNetworkSettings {
    /// Maximum packet size in bytes. If a client ever exceeds this size, they will be disconnected
    ///
    /// ## Default
    /// The default is set to 10MiB
    pub max_packet_length: usize,
    pub config: ClientConfig,
    pub bind_addr: SocketAddr,
    pub connect_addr: SocketAddr,
    pub server_name: String,
}

impl ClientNetworkSettings{
    pub fn new(config: ClientConfig, bind_addr: impl Into<SocketAddr>, connect_addr: impl Into<SocketAddr>, server_name: impl Into<String>) -> Self{
        Self{
            max_packet_length: 10 * 1024 * 1024,
            config,
            bind_addr: bind_addr.into(),
            connect_addr: connect_addr.into(),
            server_name: server_name.into()
        }
    }
}

#[derive(Clone, Debug)]
#[allow(missing_copy_implementations)]
/// Settings to configure the network, both client and server
pub struct ServerNetworkSettings {
    /// Maximum packet size in bytes. If a client ever exceeds this size, they will be disconnected
    ///
    /// ## Default
    /// The default is set to 10MiB
    pub config: ServerConfig,
    pub addr: SocketAddr,
    pub max_packet_length: usize,
}

impl ServerNetworkSettings{
    pub fn new(config: ServerConfig, addr: impl Into<SocketAddr>) -> Self{
        Self{
            config,
            addr: addr.into(),
            max_packet_length: 10 * 1024 * 1024
        }
    }
}

async fn new_connection_to_bi_connection(mut new_connection: NewConnection) -> (Connection, Result<(SendStream, RecvStream), ConnectionError>){
    (
        new_connection.connection,
        new_connection.bi_streams.next().await.unwrap()
    )
}