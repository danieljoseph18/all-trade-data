use anyhow::Result;
use futures::StreamExt;
use log::{error, info};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tonic::metadata::AsciiMetadataValue;
use tonic::service::Interceptor;
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::{Request, Status};
use tonic_health::pb::health_client::HealthClient;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
};

use crate::utils::PUMP_SWAP_PROGRAM_ID;

const MAX_RECONNECT_ATTEMPTS: u32 = 10;
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 60;

/// Initialize the gRPC connection and return the JoinHandle for the connection manager task.
pub async fn init_grpc_connection(
    handler: impl Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()>
        + Clone
        + Send
        + Sync
        + 'static,
) -> Result<tokio::task::JoinHandle<()>> {
    let endpoint = env::var("GRPC_ENDPOINT").expect("GRPC_ENDPOINT must be set");

    let mut filters = HashMap::new();
    filters.insert(
        "pump_swap_filter".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            signature: None,
            account_include: vec![PUMP_SWAP_PROGRAM_ID.to_string()],
            account_exclude: vec![],
            account_required: vec![],
        },
    );

    let mut request = SubscribeRequest::default();
    request.transactions = filters;
    request.commitment = Some(CommitmentLevel::Confirmed as i32);

    let handle = tokio::spawn(manage_connection_with_retry(endpoint, request, handler));

    Ok(handle)
}

async fn manage_connection_with_retry<F>(
    endpoint: String,
    request: SubscribeRequest,
    handler: F,
) where
    F: Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()> + Clone + Send + Sync + 'static,
{
    let mut attempt_count = 0;
    let mut backoff_secs = INITIAL_BACKOFF_SECS;

    loop {
        info!(
            "[GRPC] Connecting to gRPC stream (attempt: {})",
            attempt_count + 1
        );

        match setup_and_process_stream(&endpoint, request.clone(), handler.clone()).await {
            Ok(_) => {
                attempt_count = 0;
                backoff_secs = INITIAL_BACKOFF_SECS;
                info!("[GRPC] gRPC stream disconnected, reconnecting...");
            }
            Err(e) => {
                attempt_count += 1;
                error!(
                    "[GRPC] Error in gRPC stream connection (attempt: {}): {:?}",
                    attempt_count, e
                );

                if attempt_count >= MAX_RECONNECT_ATTEMPTS {
                    error!(
                        "[GRPC] Giving up reconnecting after {} attempts, resetting with max backoff",
                        attempt_count
                    );
                    attempt_count = 0;
                    backoff_secs = MAX_BACKOFF_SECS;
                }
            }
        }

        info!("[GRPC] Waiting {}s before reconnecting", backoff_secs);
        sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

async fn setup_and_process_stream<F>(
    endpoint: &str,
    request: SubscribeRequest,
    handler: F,
) -> Result<()>
where
    F: Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()> + Clone + Send + Sync + 'static,
{
    let mut channel_builder = Channel::from_shared(endpoint.to_string())?;
    if endpoint.starts_with("https://") {
        channel_builder = channel_builder.tls_config(ClientTlsConfig::new().with_native_roots())?;
    }
    let channel = channel_builder.connect().await?;

    let interceptor = GrpcInterceptor::from_env();
    let health_client = HealthClient::with_interceptor(channel.clone(), interceptor.clone());
    let geyser_client =
        yellowstone_grpc_proto::geyser::geyser_client::GeyserClient::with_interceptor(
            channel,
            interceptor,
        );
    let mut client = GeyserGrpcClient::new(health_client, geyser_client);

    let stream = client.subscribe_once(request).await?;

    process_stream(stream, handler).await;

    Ok(())
}

async fn process_stream<S, F>(mut stream: S, handler: F)
where
    S: futures::Stream<
            Item = Result<yellowstone_grpc_proto::geyser::SubscribeUpdate, tonic::Status>,
        > + Unpin,
    F: Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()> + Clone + Send + Sync + 'static,
{
    info!("[GRPC] gRPC stream started");

    let handler = Arc::new(handler);

    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => match msg.update_oneof {
                Some(
                    yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Transaction(tx),
                ) => {
                    if let Some(transaction_info) = tx.transaction {
                        let slot = tx.slot;
                        if let Err(err) = handler(transaction_info, slot) {
                            error!("Handler error: {:?}", err);
                        }
                    }
                }
                _ => {}
            },
            Err(error) => {
                error!("[GRPC] Stream error: {:?}", error);
                break;
            }
        }
    }

    info!("[GRPC] gRPC stream closed");
}

#[derive(Clone)]
enum GrpcInterceptor {
    Noop,
    Token(AsciiMetadataValue),
}

impl GrpcInterceptor {
    fn from_env() -> Self {
        match env::var("GRPC_TOKEN") {
            Ok(token) => match token.parse::<AsciiMetadataValue>() {
                Ok(value) => {
                    info!("GRPC_TOKEN configured for authentication");
                    Self::Token(value)
                }
                Err(e) => {
                    error!("Invalid GRPC_TOKEN value: {}, falling back to no auth", e);
                    Self::Noop
                }
            },
            Err(_) => Self::Noop,
        }
    }
}

impl Interceptor for GrpcInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Self::Token(token) = self {
            request.metadata_mut().insert("x-token", token.clone());
        }
        Ok(request)
    }
}
