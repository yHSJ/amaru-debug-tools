use amaru_kernel::Point;

/// Struct to hold the critical header and block information for comparison.
#[derive(Debug, Clone)]
pub struct HeaderInfo {
    pub slot: u64,
    pub hash: Vec<u8>,
    // The Point required to fetch the full block later
    pub point: Point,
}
