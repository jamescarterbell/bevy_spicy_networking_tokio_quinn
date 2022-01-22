use bevy::app::ScheduleRunnerSettings;
use bevy::prelude::*;
use bevy_spicy_networking::{ConnectionId, NetworkData, NetworkServer, ServerNetworkEvent};
use std::net::{SocketAddr, IpAddr};
use std::str::FromStr;
use std::time::Duration;

use bevy_spicy_networking_tokio_quinn::{TokioQuinnServerProvider, ServerNetworkSettings, ServerConfig};

mod shared;

use shared::configure_server;

fn main() {
    let mut app = App::new();
    app.insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
        1.0 / 60.0,
    )));
    app.add_plugins(MinimalPlugins);
    app.add_plugin(bevy::log::LogPlugin::default());

    // Before we can register the potential message types, we
    // need to add the plugin
    app.add_plugin(bevy_spicy_networking::ServerPlugin::<TokioQuinnServerProvider>::default());

    // A good way to ensure that you are not forgetting to register
    // any messages is to register them where they are defined!
    shared::server_register_network_messages(&mut app);

    app.add_startup_system(setup_networking);
    app.add_system(handle_connection_events);
    app.add_system(handle_messages);
    app.insert_resource(ServerNetworkSettings::new(
        configure_server().0,
        SocketAddr::from_str("127.0.0.1:8080").unwrap()
    ));

    app.run();
}

// On the server side, you need to setup networking. You do not need to do so at startup, and can start listening
// at any time.
fn setup_networking(mut net: ResMut<NetworkServer<TokioQuinnServerProvider>>, settings: Res<ServerNetworkSettings>) {
    let ip_address = "127.0.0.1".parse().expect("Could not parse ip address");

    info!("Address of the server: {}", ip_address);

    let socket_address = SocketAddr::new(ip_address, 9999);

    match net.listen(&settings) {
        Ok(_) => (),
        Err(err) => {
            error!("Could not start listening: {}", err);
            panic!();
        }
    }

    info!("Started listening for new connections!");
}

#[derive(Component)]
struct Player(ConnectionId);

fn handle_connection_events(
    mut commands: Commands,
    net: Res<NetworkServer<TokioQuinnServerProvider>>,
    mut network_events: EventReader<ServerNetworkEvent>,
) {
    for event in network_events.iter() {
        if let ServerNetworkEvent::Connected(conn_id) = event {
            commands.spawn_bundle((Player(*conn_id),));

            // Broadcasting sends the message to all connected players! (Including the just connected one in this case)
            net.broadcast(shared::NewChatMessage {
                name: String::from("SERVER"),
                message: format!("New user connected; {}", conn_id),
            });
            info!("New player connected: {}", conn_id);
        }
    }
}

// Receiving a new message is as simple as listening for events of `NetworkData<T>`
fn handle_messages(
    mut new_messages: EventReader<NetworkData<shared::UserChatMessage>>,
    net: Res<NetworkServer<TokioQuinnServerProvider>>,
) {
    for message in new_messages.iter() {
        let user = message.source();

        info!("Received message from user: {}", message.message);

        net.broadcast(shared::NewChatMessage {
            name: format!("{}", user),
            message: message.message.clone(),
        });
    }
}