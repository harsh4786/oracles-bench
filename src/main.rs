use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use futures::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, MaybeTlsStream};
use tungstenite::protocol::Message;
use url::Url;

use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestFilterAccounts,
};

pub const GEYSER_URL: &str = "http://45.250.255.183:10000";

#[tokio::main]
async fn main() {
    let pyth_hermes_ws_url = "wss://hermes.pyth.network/ws";

    println!("Connecting to Pyth Hermes...");
    let pyth_task = tokio::spawn(connect_and_benchmark_pyth(pyth_hermes_ws_url));

    println!("Connecting to Geyser...");
    let geyser_task = tokio::spawn(benchmark_drift_geyser(GEYSER_URL));

    // Run both benchmarks concurrently.
    let _ = tokio::join!(pyth_task, geyser_task);
}

/// Connects to the Pyth Hermes WebSocket, subscribes to a SOL/USD feed,
/// and then benchmarks incoming messages.
async fn connect_and_benchmark_pyth(pyth_url: &str) {
    // Parse the URL and establish a secure WebSocket connection.
    let url = Url::parse(pyth_url).expect("Invalid WebSocket URL");
    let (ws_stream, _response) = match connect_async(url).await {
        Ok(res) => res,
        Err(e) => {
            eprintln!("Failed to connect to Pyth Hermes: {}", e);
            return;
        }
    };
    println!("WebSocket connection established for Pyth Hermes");

    // Split into write and read halves.
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Send subscription request (using the SOL/USD feed ID).
    let subscription_msg = serde_json::json!({
        "type": "subscribe",
        "ids": ["e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43"]
    });
    if let Err(e) = ws_write.send(Message::Text(subscription_msg.to_string())).await {
        eprintln!("Failed to send subscription message: {}", e);
    }

    // Spawn the benchmark task to process incoming messages.
    let benchmark_handle = tokio::spawn(async move {
        benchmark_pyth_hermes(&mut ws_read).await;
    });

    // Spawn a heartbeat task that sends a Ping every 30 seconds.
    let heartbeat_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = ws_write.send(Message::Ping(vec![])).await {
                eprintln!("Failed to send Ping: {}", e);
                break;
            }
            sleep(Duration::from_secs(30)).await;
        }
    });

    if let Err(e) = benchmark_handle.await {
        eprintln!("Pyth Hermes benchmark task failed: {:?}", e);
    }
    // Abort heartbeat when done.
    heartbeat_handle.abort();
}

/// Reads messages from the Pyth Hermes WebSocket and benchmarks message rate and latency.
async fn benchmark_pyth_hermes(
    ws_read: &mut SplitStream<tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
) {
    const SAMPLE_DURATION: Duration = Duration::from_secs(60);
    let start_time = Instant::now();
    let mut last_instant = Instant::now();
    let mut message_count: u64 = 0;
    let mut latencies = Vec::new();

    println!("Starting Pyth Hermes subscription benchmark...");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws_read.next()).await {
            Ok(Some(Ok(msg))) => {
                let now = Instant::now();
                latencies.push(now.duration_since(last_instant));
                last_instant = now;
                message_count += 1;

                // if let Message::Text(text) = msg {
                //     println!("[Pyth Hermes] Received: {}", text);
                // }
            }
            Ok(Some(Err(e))) => {
                eprintln!("Error reading from Pyth Hermes: {}", e);
            }
            Ok(None) => {
                println!("Pyth Hermes stream closed.");
                break;
            }
            Err(_) => {
                println!("No messages received from Pyth Hermes for 5 seconds.");
            }
        }

        if start_time.elapsed() >= SAMPLE_DURATION {
            let elapsed_secs = start_time.elapsed().as_secs_f64();
            let msg_per_sec = message_count as f64 / elapsed_secs;
            let avg_latency = if !latencies.is_empty() {
                let sum: Duration = latencies.iter().sum();
                sum / (latencies.len() as u32)
            } else {
                Duration::ZERO
            };

            println!(
                "\n--- Pyth Hermes Wss Benchmark ---\n\
                 Duration: {:.2} s\n\
                 Messages: {}\n\
                 Messages/sec: {:.2}\n\
                 Avg Latency: {:.2?}\n",
                elapsed_secs, message_count, msg_per_sec, avg_latency
            );
            break;
        }
    }
    println!("Pyth Hermes benchmark finished.");
}

/// Connects to the Geyser service and benchmarks the incoming account updates.
async fn benchmark_drift_geyser(geyser_url: &'static str) {
    let client_builder = GeyserGrpcClient::build_from_static(geyser_url);
    let start_time = Instant::now();
    let mut last_instant = Instant::now();
    let mut message_count: u64 = 0;
    let mut latencies = Vec::new();

    // Build subscription request with the SOL token program account.
    let mut accounts = HashMap::new();
    accounts.insert(
        "client".to_owned(),
        SubscribeRequestFilterAccounts {
            account: vec!["So11111111111111111111111111111111111111112".to_string()],
            ..SubscribeRequestFilterAccounts::default()
        },
    );
    let request = SubscribeRequest {
        accounts,
        ..SubscribeRequest::default()
    };

    // Connect and subscribe.
    let mut client = client_builder.connect().await.unwrap();
    let (_, mut stream) = client.subscribe_with_request(Some(request)).await.unwrap();

    println!("Starting Geyser subscription benchmark...");
    loop {
        // Use a timeout to avoid waiting indefinitely.
        let result = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        if let Ok(Some(Ok(update))) = result {
            let now = Instant::now();
            latencies.push(now.duration_since(last_instant));
            last_instant = now;
            message_count += 1;

            // if let Some(UpdateOneof::Account(ref account_update)) = update.update_oneof {
            //     println!("[Geyser] Received account update: {:?}", account_update);
            // }
        } else if result.is_err() {
            println!("No messages received from Geyser for 5 seconds.");
        } else {
            println!("Geyser stream closed.");
            break;
        }

        if start_time.elapsed() >= Duration::from_secs(60) {
            let elapsed_secs = start_time.elapsed().as_secs_f64();
            let msg_per_sec = message_count as f64 / elapsed_secs;
            let avg_latency = if !latencies.is_empty() {
                let sum: Duration = latencies.iter().sum();
                sum / (latencies.len() as u32)
            } else {
                Duration::ZERO
            };

            println!(
                "\n--- Drift Oracle Geyser Benchmark ---\n\
                 Duration: {:.2} s\n\
                 Messages: {}\n\
                 Messages/sec: {:.2}\n\
                 Avg Latency: {:.2?}\n",
                elapsed_secs, message_count, msg_per_sec, avg_latency
            );
            break;
        }
    }
    println!("Geyser benchmark finished.");
}

/// Connects to the Geyser service and benchmarks the incoming account updates.
async fn benchmark_kamino_geyser(geyser_url: &'static str) {
    let client_builder = GeyserGrpcClient::build_from_static(geyser_url);
    let start_time = Instant::now();
    let mut last_instant = Instant::now();
    let mut message_count: u64 = 0;
    let mut latencies = Vec::new();

    // Build subscription request with the SOL token program account.
    let mut accounts = HashMap::new();
    accounts.insert(
        "client".to_owned(),
        SubscribeRequestFilterAccounts {
            account: vec!["So11111111111111111111111111111111111111112".to_string()],
            ..SubscribeRequestFilterAccounts::default()
        },
    );
    let request = SubscribeRequest {
        accounts,
        ..SubscribeRequest::default()
    };

    // Connect and subscribe.
    let mut client = client_builder.connect().await.unwrap();
    let (_, mut stream) = client.subscribe_with_request(Some(request)).await.unwrap();

    println!("Starting Geyser subscription benchmark...");
    loop {
        // Use a timeout to avoid waiting indefinitely.
        let result = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        if let Ok(Some(Ok(update))) = result {
            let now = Instant::now();
            latencies.push(now.duration_since(last_instant));
            last_instant = now;
            message_count += 1;

            // if let Some(UpdateOneof::Account(ref account_update)) = update.update_oneof {
            //     println!("[Geyser] Received account update: {:?}", account_update);
            // }
        } else if result.is_err() {
            println!("No messages received from Geyser for 5 seconds.");
        } else {
            println!("Geyser stream closed.");
            break;
        }

        if start_time.elapsed() >= Duration::from_secs(60) {
            let elapsed_secs = start_time.elapsed().as_secs_f64();
            let msg_per_sec = message_count as f64 / elapsed_secs;
            let avg_latency = if !latencies.is_empty() {
                let sum: Duration = latencies.iter().sum();
                sum / (latencies.len() as u32)
            } else {
                Duration::ZERO
            };

            println!(
                "\n--- Drift Oracle Geyser Benchmark ---\n\
                 Duration: {:.2} s\n\
                 Messages: {}\n\
                 Messages/sec: {:.2}\n\
                 Avg Latency: {:.2?}\n",
                elapsed_secs, message_count, msg_per_sec, avg_latency
            );
            break;
        }
    }
    println!("Geyser benchmark finished.");
}