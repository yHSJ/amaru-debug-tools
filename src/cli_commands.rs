use anyhow::{Context, Result};
use futures_util::future::join;
pub use pallas_crypto::{
    hash::{Hash, Hasher},
    key::ed25519,
};
use pallas_network::facades::PeerClient;
use pallas_network::miniprotocols::blockfetch::{Body, Client as BlockfetchClient};
use pallas_network::miniprotocols::chainsync::NextResponse;

use futures_executor::block_on;
use std::path::PathBuf;
use std::{collections::HashMap, io::BufReader};
use std::{
    collections::HashSet,
    io::{BufWriter, Write},
};
use std::{fs::File, io::BufRead};
use tokio::time::sleep;

// --- Imports from amaru-network and amaru-kernel ---
use amaru_kernel::{cbor, MintedBlock, OriginalHash, PoolId};
use amaru_kernel::{peer::Peer, Point};
use amaru_network::chain_sync_client::{to_traverse, ChainSyncClient};
use amaru_network::point::to_network_point;
// ---------------------------------------------------

use crate::cli::{ForkTracerArgs, TracerDiffArgs};
use crate::cli::{ProtocolVersionArgs, SlotDivergenceArgs};
use crate::data_types::HeaderInfo;

// --- Core Network Logic (Helper Functions) ---

pub fn issuer_to_pool_id(issuer: &ed25519::PublicKey) -> PoolId {
    // The pool ID is the blake2b-224 hash (28 bytes) of the cold verification key.
    Hasher::<224>::hash(issuer.as_ref())
}

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
    client.fetch_single(network_point).await.with_context(|| {
        format!(
            "Failed to fetch block for point {:?} from {}",
            point, peer_name
        )
    })
}

/// Connects to a peer, extracts both ChainSync and BlockFetch clients, and performs intersection.
async fn connect_and_intersect(
    host: &str,
    magic: u64,
    start_point: Point,
) -> Result<(ChainSyncClient, BlockfetchClient)> {
    let peer = Peer::new(host);
    tracing::info!("Connecting to peer: {}", peer.name);

    let PeerClient {
        chainsync,
        blockfetch,
        ..
    } = pallas_network::facades::PeerClient::connect(peer.name.as_str(), magic)
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
            let (mut _chainsync, mut blockfetch) =
                connect_and_intersect(host_a, 764824073, good.point.clone()).await?;
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

fn print_last_good_info(last_good: &Option<HeaderInfo>) {
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
    const MAGIC: u64 = 764824073;

    let start_hash =
        hex::decode(&args.hash).context("Failed to decode start block hash from hex string")?;
    let start_point = Point::Specific(args.slot, start_hash);

    tracing::info!("Starting divergence check from Slot: {}", args.slot);
    tracing::info!("Relay A: {}", args.relay_a);
    tracing::info!("Relay B: {}", args.relay_b);

    // --- Concurrent Connection and Intersection ---
    let (res_a, res_b) = join(
        connect_and_intersect(&args.relay_a, MAGIC, start_point.clone()),
        connect_and_intersect(&args.relay_b, MAGIC, start_point),
    )
    .await;

    let (mut chainsync_a, mut blockfetch_a) =
        res_a.context("Client A failed to connect or intersect")?;
    let (mut chainsync_b, mut blockfetch_b) =
        res_b.context("Client B failed to connect or intersect")?;

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
        )
        .await;

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
                        &args.relay_a,
                        &a,
                        &block_a_res,
                        &args.relay_b,
                        &b,
                        &block_b_res,
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
                        &args.relay_a,
                        &a,
                        &block_a_res,
                        &args.relay_b,
                        &b,
                        &block_b_res,
                    );
                    break;
                }
            }
            (None, None) => {
                let last_slot = last_good_header
                    .as_ref()
                    .map(|h| h.slot)
                    .unwrap_or(args.slot);
                tracing::info!(
                    "Both peers are Awaiting. Sync complete up to slot {}.",
                    last_slot
                );
                tracing::info!("--- SYNC COMPLETE (Both Peers Awaiting) ---");
                print_last_good_info(&last_good_header);
                break;
            }
            (Some(a), None) => {
                let last_slot = last_good_header
                    .as_ref()
                    .map(|h| h.slot)
                    .unwrap_or(args.slot);
                tracing::warn!("Client B ({}) is Awaiting (Slot {}), but Client A ({}) is still sending blocks (Slot {}).", &args.relay_b, last_slot, &args.relay_a, a.slot);
                tracing::info!("--- SYNC COMPLETE (One Peer Awaiting) ---");
                print_last_good_info(&last_good_header);
                break;
            }
            (None, Some(b)) => {
                let last_slot = last_good_header
                    .as_ref()
                    .map(|h| h.slot)
                    .unwrap_or(args.slot);
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
    let PeerClient { .. } =
        pallas_network::facades::PeerClient::connect(peer.name.as_str(), args.magic)
            .await
            .context(format!("Failed to connect to peer: {}", host))?;

    tracing::info!("\n#####################################################");
    tracing::info!("PROTOCOL VERSION CHECK:");
    tracing::info!("Successfully connected and completed handshake.");
    tracing::info!(
        "Highest Protocol Version supported by {} is confirmed by successful Handshake.",
        host
    );
    tracing::info!("#####################################################");
    Ok(())
}

pub async fn run_fork_tracer(args: ForkTracerArgs) -> Result<()> {
    let start_hash =
        hex::decode(&args.hash).context("Failed to decode start block hash from hex string")?;
    let start_point = Point::Specific(args.slot, start_hash);

    tracing::info!(
        "Starting fork trace on {} from Slot: {}",
        args.relay,
        args.slot
    );

    // We only need the ChainSync client for traversal, but connect_and_intersect returns both.
    let (mut chainsync, _blockfetch) =
        connect_and_intersect(&args.relay, args.magic, start_point.clone())
            .await
            .context("Failed to connect and intersect at fork point")?;

    let mut file_writer = FileWriter::new(args.output_file.as_ref());

    let mut blocks_found = 0;
    // MODIFIED: Changed to HashMap<String, u32> to track counts
    let mut pool_id_counts: HashMap<String, u32> = HashMap::new();

    tracing::info!("--- Tracing Divergent Chain ---");

    loop {
        // Request the next header
        let response_result = chainsync.request_next().await;

        let response = match response_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("ChainSync error during traversal: {}", e);
                break;
            }
        };

        match response {
            NextResponse::RollForward(hd, _tip) => {
                // Decode Header and Extract Pool ID
                let header = to_traverse(&hd).context("Failed to decode header")?;

                let pool_id_hex_option: Option<String> =
                    header.issuer_vkey().and_then(|vkey_bytes| {
                        // FIX E0308: Convert &[u8] to ed25519::PublicKey before hashing
                        ed25519::PublicKey::try_from(vkey_bytes)
                            .ok()
                            .map(|vkey| issuer_to_pool_id(&vkey))
                            .map(|pool_id| hex::encode(pool_id)) // <-- Hex encoding happens here
                    });

                // MODIFIED: Update count in the HashMap instead of inserting into a HashSet
                if let Some(ref hex_string) = pool_id_hex_option {
                    *pool_id_counts.entry(hex_string.clone()).or_insert(0) += 1;
                }

                tracing::info!(
                    "  [+] Block Slot: {} | Hash: {} | Pool ID: {:?}",
                    header.slot(),
                    hex::encode(header.hash()),
                    pool_id_hex_option // Use the correctly encoded Option<String>
                );

                file_writer.write(hex::encode(header.hash()));

                blocks_found += 1;
            }
            NextResponse::RollBackward(point, tip) => {
                // Possible if the network forces another rollback mid-traversal
                tracing::warn!("Chain rolled back to point: {:?} (Tip: {:?})", point, tip);
                // Continue the loop from the new point
            }
            NextResponse::Await => {
                // --- MODIFIED: Final Reporting Logic ---

                // 1. Convert HashMap entries into a vector of (count, pool_id) tuples
                let mut sorted_pools: Vec<(u32, String)> = pool_id_counts
                    .into_iter()
                    .map(|(pool_id, count)| (count, pool_id))
                    .collect();

                // 2. Sort the vector by count. Ascending order puts the most frequent pools at the bottom.
                sorted_pools.sort_by(|a, b| a.0.cmp(&b.0));

                // 3. Print the sorted list
                tracing::info!(
                    "\n--- Pool ID Block Distribution ({} unique pools) ---",
                    sorted_pools.len()
                );
                tracing::info!("(Pools are sorted by block count, ascending: least frequent at top, most frequent at bottom)");
                for (count, pool_id) in sorted_pools.iter() {
                    // Use format string to align the count nicely
                    println!("  Blocks: {:<4} | Pool ID: {}", count, pool_id);
                }
                tracing::info!("----------------------------------------------------");

                tracing::info!("--- Reached Tip ---");
                tracing::info!(
                    "Traversal complete. Found {} blocks on the divergent chain.",
                    blocks_found
                );
                break;
            }
        }
    }

    Ok(())
}

pub async fn run_transaction_tracer(args: ForkTracerArgs) -> Result<()> {
    let start_hash =
        hex::decode(&args.hash).context("Failed to decode start block hash from hex string")?;
    let start_point = Point::Specific(args.slot, start_hash);

    tracing::info!(
        "Starting transaction trace on {} from Slot: {}",
        args.relay,
        args.slot
    );

    // We only need the ChainSync client for traversal, but connect_and_intersect returns both.
    let (mut chainsync, mut block_fetch) =
        connect_and_intersect(&args.relay, args.magic, start_point.clone())
            .await
            .context("Failed to connect and intersect at fork point")?;

    let mut file_writer = FileWriter::new(
        args.output_file
            .inspect(|path| tracing::info!("attemping to dump transaction IDs to {path:?}..."))
            .as_ref(),
    );

    let mut transactions_found = 0;
    let mut pool_id_counts: HashMap<String, usize> = HashMap::new();

    tracing::info!("--- Tracing Chain ---");

    loop {
        // Request the next header
        let response_result = chainsync.request_next().await;

        let response = match response_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("ChainSync error during traversal: {}", e);
                break;
            }
        };
        match response {
            NextResponse::RollForward(hd, tip) => {
                // Decode Header and Extract Pool ID
                let header = to_traverse(&hd).context("Failed to decode header")?;

                let block_bytes = block_fetch
                    .fetch_single(tip.0)
                    .await
                    .context("Failed to fetch block")?;

                let (_, block): (u16, MintedBlock<'_>) =
                    cbor::decode(&block_bytes).context("failed to decode block bytes")?;

                let txs: Vec<Hash<32>> = block
                    .transaction_bodies
                    .iter()
                    .map(|tx| tx.original_hash())
                    .collect();

                let pool_id_hex_option: Option<String> =
                    header.issuer_vkey().and_then(|vkey_bytes| {
                        // FIX E0308: Convert &[u8] to ed25519::PublicKey before hashing
                        ed25519::PublicKey::try_from(vkey_bytes)
                            .ok()
                            .map(|vkey| issuer_to_pool_id(&vkey))
                            .map(|pool_id| hex::encode(pool_id)) // <-- Hex encoding happens here
                    });

                if let Some(ref hex_string) = pool_id_hex_option {
                    *pool_id_counts.entry(hex_string.clone()).or_insert(0) += txs.len();
                }

                tracing::info!(
                    "  [+] Block Slot: {} | Transaction Count: {} | Pool ID: {:?}",
                    header.slot(),
                    txs.len(),
                    pool_id_hex_option
                );

                file_writer.write(
                    txs.iter()
                        .map(|tx_id| hex::encode(tx_id))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );

                transactions_found += txs.len();
            }
            NextResponse::RollBackward(point, tip) => {
                // Possible if the network forces another rollback mid-traversal
                tracing::warn!("Chain rolled back to point: {:?} (Tip: {:?})", point, tip);
                // Continue the loop from the new point
            }
            NextResponse::Await => {
                // --- MODIFIED: Final Reporting Logic ---

                // 1. Convert HashMap entries into a vector of (count, pool_id) tuples
                let mut sorted_pools: Vec<(usize, String)> = pool_id_counts
                    .into_iter()
                    .map(|(pool_id, count)| (count, pool_id))
                    .collect();

                // 2. Sort the vector by count. Ascending order puts the most frequent pools at the bottom.
                sorted_pools.sort_by(|a, b| a.0.cmp(&b.0));

                // 3. Print the sorted list
                tracing::info!(
                    "\n--- Pool ID Block Distribution ({} unique pools) ---",
                    sorted_pools.len()
                );
                tracing::info!("(Pools are sorted by transaction count, ascending: least frequent at top, most frequent at bottom)");
                for (count, pool_id) in sorted_pools.iter() {
                    // Use format string to align the count nicely
                    println!("  Tranasctions: {:<4} | Pool ID: {}", count, pool_id);
                }
                tracing::info!("----------------------------------------------------");

                tracing::info!("--- Reached Tip ---");
                tracing::info!(
                    "Traversal complete. Found {} transactions on the provided chain.",
                    transactions_found
                );
                break;
            }
        }
    }

    Ok(())
}

pub fn tracer_diff(args: TracerDiffArgs) -> Result<()> {
    tracing::info!("--- Finding Diff in Tracer Outputs ---");
    let diff = diff_files(args.ko_file, args.ok_file)?;
    tracing::info!("Diff complete. Found {} transactions in the provided KO file that were not in the OK file.", diff.len());
    let out_file = File::create(&args.output_file).context("Failed to create output file")?;
    let mut writer = BufWriter::new(out_file);
    writeln!(writer, "{}", diff.join("\n"))?;
    tracing::info!("Wrote full diff to {:?}", args.output_file);
    Ok(())
}

/// Line-by-line diff of A-B
pub fn diff_files(path_a: PathBuf, path_b: PathBuf) -> Result<Vec<String>> {
    let file_a = File::open(&path_a).context("Failed to open path a")?;
    let file_b = File::open(&path_b).context("Failed to open path b")?;

    let reader_a = BufReader::new(file_a);
    let reader_b = BufReader::new(file_b);

    let lines_a: Vec<String> = reader_a.lines().collect::<Result<_, _>>()?;
    let lines_b: HashSet<String> = reader_b.lines().collect::<Result<_, _>>()?;

    let mut diff = vec![];

    for line in lines_a {
        if !lines_b.contains(&line) {
            diff.push(line);
        }
    }

    Ok(diff)
}

struct FileWriter {
    writer: Option<BufWriter<File>>,
}

impl FileWriter {
    fn new(path: Option<&PathBuf>) -> Self {
        let writer = path
            .and_then(|p| {
                File::create(p)
                    .inspect_err(|err| tracing::warn!("Failed to open path {p:?}: {err}"))
                    .ok()
            })
            .map(BufWriter::new);

        Self { writer }
    }

    fn write<T: AsRef<str>>(&mut self, data: T) {
        if let Some(ref mut w) = self.writer {
            writeln!(w, "{}", data.as_ref()).ok();
        }
    }
}
