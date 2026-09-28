//! Bound a bulk-IN queue to one known-length IIOD payload.
use crate::{Error, Result};

pub(super) const TRANSFER_BYTES: usize = 64 * 1024;
pub(super) const TRANSFER_COUNT: usize = 4;

pub(super) struct ReadQueue {
    remaining: usize,
    packet: usize,
    reserved: usize,
    requests: [usize; TRANSFER_COUNT],
    head: usize,
    count: usize,
}

impl ReadQueue {
    pub(super) fn new(length: usize, packet: usize) -> Self {
        Self {
            remaining: length,
            packet,
            reserved: 0,
            requests: [0; TRANSFER_COUNT],
            head: 0,
            count: 0,
        }
    }

    /// Reserve only bytes inside the current payload. A final partial USB
    /// packet is rounded up only once all earlier transfers have completed.
    pub(super) fn next_request(&mut self) -> Option<usize> {
        if self.count == TRANSFER_COUNT {
            return None;
        }
        let available = self.remaining.saturating_sub(self.reserved);
        let length = if available >= self.packet {
            available.min(TRANSFER_BYTES) / self.packet * self.packet
        } else if available != 0 && self.count == 0 {
            self.packet
        } else {
            return None;
        };
        self.reserved += length;
        self.requests[(self.head + self.count) % TRANSFER_COUNT] = length;
        self.count += 1;
        Some(length)
    }

    /// Return how much of a completion belongs to this payload. Any excess
    /// belongs to subsequent framing and must be retained by the transport.
    pub(super) fn complete(&mut self, actual: usize) -> Result<usize> {
        if self.count == 0 {
            return Err(Error::Protocol("unexpected USB completion"));
        }
        let requested = self.requests[self.head];
        self.head = (self.head + 1) % TRANSFER_COUNT;
        self.count -= 1;
        if actual > requested {
            return Err(Error::Protocol("USB completion exceeds request"));
        }
        self.reserved -= requested;
        let consumed = actual.min(self.remaining);
        self.remaining -= consumed;
        Ok(consumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn queue_stays_inside_payload_with_short_packets_and_zlps() {
        for packet in [64, 512, 1024] {
            for length in [1, packet - 1, packet, packet + 1, 262_144, 1_048_575] {
                let mut q = ReadQueue::new(length, packet);
                let mut received = 0;
                let mut completions = 0;
                while received < length {
                    while let Some(n) = q.next_request() {
                        assert!(n.is_multiple_of(packet));
                        assert!(n <= TRANSFER_BYTES);
                    }
                    assert!(q.count != 0);
                    assert!(q.count <= TRANSFER_COUNT);
                    assert!(q.reserved <= q.remaining || (q.count == 1 && q.remaining < packet));
                    // ZLPs and arbitrary short transfers must reclaim unused
                    // reservations without leaving reads beyond the payload.
                    let request = q.requests[q.head];
                    let actual = match completions % 5 {
                        0 => 0,
                        1 => request.min(37),
                        _ => request.min(q.remaining),
                    };
                    received += q.complete(actual).unwrap();
                    completions += 1;
                }
                assert_eq!(received, length);
                assert!(q.count == 0);
                assert_eq!(q.next_request(), None);
            }
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn pipelines_four_transfers_and_preserves_final_overread() {
        let mut q = ReadQueue::new(TRANSFER_BYTES * 8, 512);
        for _ in 0..TRANSFER_COUNT {
            assert_eq!(q.next_request(), Some(TRANSFER_BYTES));
        }
        assert_eq!(q.next_request(), None);
        assert_eq!(q.complete(TRANSFER_BYTES).unwrap(), TRANSFER_BYTES);
        assert_eq!(q.next_request(), Some(TRANSFER_BYTES));
        let mut q = ReadQueue::new(3, 512);
        assert_eq!(q.next_request(), Some(512));
        assert_eq!(q.next_request(), None);
        assert_eq!(q.complete(12).unwrap(), 3);
        assert!(q.count == 0);
    }
}
