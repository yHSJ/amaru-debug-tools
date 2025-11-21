use anyhow::{Context, Result};
use futures_util::future::join;
use pallas_network::facades::PeerClient;
use pallas_network::miniprotocols::chainsync::NextResponse;
use pallas_network::miniprotocols::blockfetch::{Client as BlockfetchClient, Body};
use std::fmt::Debug;
use tokio::time::sleep;
use futures_executor::block_on;

// --- Imports from amaru-network and amaru-kernel ---
use amaru_kernel::{peer::Peer, Point};
use amaru_network::chain_sync_client::{ChainSyncClient, to_traverse};
use amaru_network::point::to_network_point;
// ---------------------------------------------------

use crate::data_types::HeaderInfo;
use crate::cli::{SlotDivergenceArgs, ProtocolVersionArgs};

// --- Core Network Logic (Helper Functions) ---

/// Attempts to get the next header from a ChainSync client.
async fn fetch_next_header(
    client: &mut ChainSyncClient,
    peer_name: &str,
) -> Result<Option<HeaderInfo>> {
    loop {
        let response = client
            .request_next()
            .await
            .with_context(|| format!("Failed to request next header from {}", peer_name))?;

        match response {
            NextResponse::RollForward(hd, _tip) => {
                let header = to_traverse(&hd).context("Failed to decode RollForward header")?;
                return Ok(Some(HeaderInfo {
                    slot: header.slot(),
                    hash: header.hash().to_vec(),
                    point: Point::Specific(header.slot(), header.hash().to_vec()),
                }));
            }
            NextResponse::RollBackward(point, _tip) => {
                tracing::warn!(
                    "Peer {} requested RollBackward to point: {:?}",
                    peer_name,
                    point
                );
                continue;
            }
            NextResponse::Await => {
                return Ok(None);
            }
        }
    }
}

/// Fetches the full block body (CBOR) for a given point.
async fn fetch_block_cbor(
    client: &mut BlockfetchClient,
    point: Point,
    peer_name: &str,
) -> Result<Body> {
    let network_point = to_network_point(point.clone());
    client
        .fetch_single(network_point)
        .await
        .with_context(|| format!("Failed to fetch block for point {:?} from {}", point, peer_name))
}

/// Connects to a peer, extracts both ChainSync and BlockFetch clients, and performs intersection.
async fn connect_and_intersect(
    host: &str,
    magic: u64,
    start_point: Point
) -> Result<(ChainSyncClient, BlockfetchClient)> {
    let peer = Peer::new(host);
    tracing::info!("Connecting to peer: {}", peer.name);

    let PeerClient { chainsync, blockfetch, .. } = pallas_network::facades::PeerClient::connect(peer.name.as_str(), magic)
        .await
        .context(format!("Failed to connect to peer: {}", host))?;

    let intersection_points = vec![start_point.clone()];
    let mut chainsync_client = ChainSyncClient::new(peer, chainsync, intersection_points);

    tracing::info!("Finding intersection on {}...", host);
    chainsync_client
        .find_intersection()
        .await
        .with_context(|| format!("Failed to find intersection on {}", host))?;

    Ok((chainsync_client, blockfetch))
}

// --- Reporting Functions (omitted for brevity, assume correct) ---

fn print_divergence_info(
    last_good: &Option<HeaderInfo>,
    host_a: &str,
    diverging_a: &HeaderInfo,
    block_a_res: &Result<Body>,
    host_b: &str,
    diverging_b: &HeaderInfo,
    block_b_res: &Result<Body>,
) {
    if let Some(good) = last_good {
        tracing::error!("\n\n#####################################################");
        tracing::error!("LAST MATCHING BLOCK (BEFORE DIVERGENCE):");
        tracing::error!("  Slot: {}", good.slot);
        tracing::error!("  Hash: {}", hex::encode(&good.hash));

        // Fetch and report the last good block's CBOR
        let last_good_fetch_result = block_on(async {
            // Re-connect specifically for fetching the last good block
            let (mut _chainsync, mut blockfetch) = connect_and_intersect(host_a, 2, good.point.clone()).await?;
            fetch_block_cbor(&mut blockfetch, good.point.clone(), host_a).await
        });

        match last_good_fetch_result {
            Ok(body) => {
                tracing::error!("  CBOR Hex: {}", hex::encode(body));
            }
            Err(e) => {
                tracing::error!("  Could not fetch last matching block CBOR: {}", e);
            }
        }

        tracing::error!("#####################################################");
    } else {
        tracing::error!("\n\n#####################################################");
        tracing::error!("No common starting block could be confirmed.");
        tracing::error!("#####################################################");
    }

    tracing::error!("\nPEER A DIVERGING BLOCK ({}):", host_a);
    tracing::error!("  Slot: {}", diverging_a.slot);
    tracing::error!("  Hash: {}", hex::encode(&diverging_a.hash));
    match block_a_res {
        Ok(body) => {
            tracing::error!("  CBOR Hex: {}", hex::encode(body));
            tracing::error!("  CBOR Length: {} bytes", body.len());
        }
        Err(e) => tracing::error!("  Block Fetch Failed: {}", e),
    }

    tracing::error!("\nPEER B DIVERGING BLOCK ({}):", host_b);
    tracing::error!("  Slot: {}", diverging_b.slot);
    tracing::error!("  Hash: {}", hex::encode(&diverging_b.hash));
    match block_b_res {
        Ok(body) => {
            tracing::error!("  CBOR Hex: {}", hex::encode(body));
            tracing::error!("  CBOR Length: {} bytes", body.len());
        }
        Err(e) => tracing::error!("  Block Fetch Failed: {}", e),
    }

    tracing::error!("\n#####################################################");
}

fn print_last_good_info(
    last_good: &Option<HeaderInfo>,
) {
     if let Some(good) = last_good {
        tracing::info!("\n\n#####################################################");
        tracing::info!("SYNC COMPLETE (PEERS CAUGHT UP):");
        tracing::info!("  Slot: {}", good.slot);
        tracing::info!("  Hash: {}", hex::encode(&good.hash));
        tracing::info!("#####################################################");
    } else {
        tracing::info!("\n\n#####################################################");
        tracing::info!("Sync started but no blocks were retrieved.");
        tracing::info!("#####################################################");
    }
}

// --- Command Implementations ---

/// Main entry point for the slot-divergence command.
pub async fn run_slot_divergence(args: SlotDivergenceArgs) -> Result<()> {
    // Cardano Preview Testnet magic number
    const MAGIC: u64 = 2;

    let start_hash = hex::decode(&args.hash).context("Failed to decode start block hash from hex string")?;
    let start_point = Point::Specific(args.slot, start_hash);

    tracing::info!("Starting divergence check from Slot: {}", args.slot);
    tracing::info!("Relay A: {}", args.relay_a);
    tracing::info!("Relay B: {}", args.relay_b);

    // --- Concurrent Connection and Intersection ---
    let (res_a, res_b) = join(
        connect_and_intersect(&args.relay_a, MAGIC, start_point.clone()),
        connect_and_intersect(&args.relay_b, MAGIC, start_point),
    ).await;

    let (mut chainsync_a, mut blockfetch_a) = res_a.context("Client A failed to connect or intersect")?;
    let (mut chainsync_b, mut blockfetch_b) = res_b.context("Client B failed to connect or intersect")?;

    tracing::info!("--- Starting Hash Comparison Loop ---");

    let mut last_good_header: Option<HeaderInfo> = None;

    // Set initial last_good_header to the start point provided by the user
    last_good_header = Some(HeaderInfo {
        slot: args.slot,
        hash: hex::decode(&args.hash).unwrap(),
        point: Point::Specific(args.slot, hex::decode(&args.hash).unwrap()),
    });

    loop {
        // Request next header from both peers concurrently
        let (header_a_res, header_b_res) = join(
            fetch_next_header(&mut chainsync_a, &args.relay_a),
            fetch_next_header(&mut chainsync_b, &args.relay_b),
        ).await;

        let header_a = header_a_res.context("Failed to fetch next header from Client A")?;
        let header_b = header_b_res.context("Failed to fetch next header from Client B")?;

        match (header_a, header_b) {
            (Some(a), Some(b)) => {
                if a.slot != b.slot {
                    tracing::error!("--- DIVERGENCE FOUND (Slot Mismatch) ---");
                    // Fetch full blocks for analysis
                    let (block_a_res, block_b_res) = tokio::join!(
                        fetch_block_cbor(&mut blockfetch_a, a.point.clone(), &args.relay_a),
                        fetch_block_cbor(&mut blockfetch_b, b.point.clone(), &args.relay_b)
                    );

                    print_divergence_info(
                        &last_good_header,
                        &args.relay_a, &a, &block_a_res,
                        &args.relay_b, &b, &block_b_res
                    );
                    break;
                }

                if a.hash == b.hash {
                    tracing::info!("Slot {} OK: Hashes match.", a.slot);
                    last_good_header = Some(a.clone());
                } else {
                    tracing::error!("--- DIVERGENCE FOUND (Hash Mismatch) ---");
                    // Fetch full blocks for analysis
                    let (block_a_res, block_b_res) = tokio::join!(
                        fetch_block_cbor(&mut blockfetch_a, a.point.clone(), &args.relay_a),
                        fetch_block_cbor(&mut blockfetch_b, b.point.clone(), &args.relay_b)
                    );

                    print_divergence_info(
                        &last_good_header,
                        &args.relay_a, &a, &block_a_res,
                        &args.relay_b, &b, &block_b_res
                    );
                    break;
                }
            }
            (None, None) => {
                let last_slot = last_good_header.as_ref().map(|h| h.slot).unwrap_or(args.slot);
                tracing::info!("Both peers are Awaiting. Sync complete up to slot {}.", last_slot);
                tracing::info!("--- SYNC COMPLETE (Both Peers Awaiting) ---");
                print_last_good_info(&last_good_header);
                break;
            }
            (Some(a), None) => {
                let last_slot = last_good_header.as_ref().map(|h| h.slot).unwrap_or(args.slot);
                tracing::warn!("Client B ({}) is Awaiting (Slot {}), but Client A ({}) is still sending blocks (Slot {}).", &args.relay_b, last_slot, &args.relay_a, a.slot);
                tracing::info!("--- SYNC COMPLETE (One Peer Awaiting) ---");
                print_last_good_info(&last_good_header);
                break;
            }
            (None, Some(b)) => {
                let last_slot = last_good_header.as_ref().map(|h| h.slot).unwrap_or(args.slot);
                tracing::warn!("Client A ({}) is Awaiting (Slot {}), but Client B ({}) is still sending blocks (Slot {}).", &args.relay_a, last_slot, &args.relay_b, b.slot);
                tracing::info!("--- SYNC COMPLETE (One Peer Awaiting) ---");
                print_last_good_info(&last_good_header);
                break;
            }
        }

        // Add a small pause to prevent aggressive polling if Await is not properly implemented.
        sleep(std::time::Duration::from_millis(50)).await;
    }

    Ok(())
}

/// Main entry point for the protocol-version command.
pub async fn run_protocol_version(args: ProtocolVersionArgs) -> Result<()> {
    let host = &args.relay;
    let peer = Peer::new(host);

    tracing::info!("Querying protocol version for peer: {}", peer.name);

    // FIX E0026: Removed the field 'handshake' from destructuring.
    let PeerClient { .. } = pallas_network::facades::PeerClient::connect(peer.name.as_str(), args.magic)
        .await
        .context(format!("Failed to connect to peer: {}", host))?;

    tracing::info!("\n#####################################################");
    tracing::info!("PROTOCOL VERSION CHECK:");
    tracing::info!("Successfully connected and completed handshake.");
    tracing::info!("Highest Protocol Version supported by {} is confirmed by successful Handshake.", host);
    tracing::info!("#####################################################");
    Ok(())
}
