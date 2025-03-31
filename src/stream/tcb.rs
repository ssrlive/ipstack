use super::{reno::Reno, seqnum::SeqNum};
use etherparse::TcpHeader;
use std::collections::{BTreeMap, VecDeque};

// const MAX_UNACK: u32 = 1024 * 16; // 16KB
const READ_BUFFER_SIZE: usize = 1024 * 16; // 16KB

const MAX_COUNT_FOR_DUP_ACK: usize = 3; // Maximum number of duplicate ACKs before retransmission

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum TcpState {
    // Init, /* Since we always act as a server, it starts from `Listen`, so we don't use states Init & SynSent. */
    // SynSent,
    Listen,
    SynReceived,
    Established,
    FinWait1, // act as a client, actively send a farewell packet to the other side, followed with FinWait2, TimeWait, Closed
    FinWait2,
    TimeWait,
    CloseWait, // act as a server, followed with LastAck, Closed
    LastAck,
    Closed,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(super) enum PacketStatus {
    WindowUpdate,
    Invalid,
    RetransmissionRequest,
    NewPacket,
    Ack,
    KeepAlive,
    Fin,
    DuplicateAck,
    OutOfOrder,
}

#[derive(Debug)]
pub(super) struct Tcb {
    seq: SeqNum,
    ack: SeqNum,
    last_received_ack: SeqNum,
    duplicate_ack_count: usize,
    state: TcpState,
    reno: Reno,
    inflight_packets: VecDeque<InflightPacket>,
    unordered_packets: BTreeMap<SeqNum, UnorderedPacket>,
}

impl Tcb {
    pub fn update_duplicate_ack_count(&mut self, rcvd_ack: SeqNum) {
        // If the received rcvd_ack is the same as self.last_received_ack and not all data has been acknowledged (rcvd_ack < self.seq), increment the count.
        if rcvd_ack == self.last_received_ack && rcvd_ack < self.seq {
            self.duplicate_ack_count = self.duplicate_ack_count.saturating_add(1);
        } else {
            // self.last_received_ack = rcvd_ack;
            self.duplicate_ack_count = 0; // reset duplicate ACK count
        }
    }

    pub fn get_duplicate_ack_count(&self) -> usize {
        self.duplicate_ack_count
    }
}

impl Tcb {
    pub(super) fn new(ack: SeqNum, mss: usize) -> Tcb {
        #[cfg(debug_assertions)]
        let seq = 100;
        #[cfg(not(debug_assertions))]
        let seq = rand::Rng::random::<u32>(&mut rand::rng());
        Tcb {
            seq: seq.into(),
            ack,
            last_received_ack: seq.into(),
            duplicate_ack_count: 0,
            state: TcpState::Listen,
            reno: Reno::new(mss),
            inflight_packets: VecDeque::new(),
            unordered_packets: BTreeMap::new(),
        }
    }

    pub(super) fn add_unordered_packet(&mut self, seq: SeqNum, buf: Vec<u8>) {
        if seq < self.ack {
            log::warn!("Received packet seq < ack: seq = {}, ack = {}, len = {}", seq, self.ack, buf.len());
            return;
        }
        self.unordered_packets.insert(seq, UnorderedPacket::new(buf));
    }
    pub(super) fn get_available_read_buffer_size(&self) -> usize {
        READ_BUFFER_SIZE.saturating_sub(self.unordered_packets.values().map(|p| p.payload.len()).sum())
    }

    pub(super) fn get_unordered_packets(&mut self) -> Option<Vec<u8>> {
        // dbg!(self.ack);
        // for (seq,_) in self.unordered_packets.iter() {
        //     dbg!(seq);
        // }
        self.unordered_packets.remove(&self.ack).map(|p| {
            self.ack += p.payload.len() as u32;
            p.payload
        })
    }

    pub(super) fn increase_seq(&mut self) {
        self.seq += 1;
    }
    pub(super) fn get_seq(&self) -> SeqNum {
        self.seq
    }
    pub(super) fn increase_ack(&mut self) {
        self.ack += 1;
    }
    pub(super) fn get_ack(&self) -> SeqNum {
        self.ack
    }
    pub(super) fn get_last_received_ack(&self) -> SeqNum {
        self.last_received_ack
    }
    pub(super) fn change_state(&mut self, state: TcpState) {
        self.state = state;
    }
    pub(super) fn get_state(&self) -> TcpState {
        self.state
    }
    pub(super) fn update_send_window(&mut self, window: u16) {
        self.reno.set_remote_window(window as usize); // update remote window
    }
    pub(super) fn get_effective_send_window(&self) -> u16 {
        self.reno.window().min(u16::MAX as usize) as u16 // limit to u16 range
    }
    pub(crate) fn on_duplicate_ack(&mut self) {
        self.reno.on_duplicate_ack();
    }

    // call on_retransmit when TCP packet transmission timeout
    #[allow(dead_code)]
    pub(crate) fn on_retransmit(&mut self) {
        self.reno.on_retransmit();
    }
    pub(super) fn get_recv_window(&self) -> u16 {
        self.get_available_read_buffer_size() as u16
    }
    // #[inline(always)]
    // pub(super) fn buffer_size(&self, payload_len: u16) -> u16 {
    //     match MAX_UNACK - self.inflight_packets.len() as u32 {
    //         // b if b.saturating_sub(payload_len as u32 + 64) != 0 => payload_len,
    //         // b if b < 128 && b >= 4 => (b / 2) as u16,
    //         // b if b < 4 => b as u16,
    //         // b => (b - 64) as u16,
    //         b if b >= payload_len as u32 * 2 && b > 0 => payload_len,
    //         b if b < 4 => b as u16,
    //         b => (b / 2) as u16,
    //     }
    // }

    pub(super) fn check_pkt_type(&self, tcp_header: &TcpHeader, payload: &[u8]) -> PacketStatus {
        let rcvd_ack = SeqNum(tcp_header.acknowledgment_number);
        let rcvd_seq = SeqNum(tcp_header.sequence_number);
        let rcvd_window = tcp_header.window_size;
        let has_fin = tcp_header.fin;
        let has_ack = tcp_header.ack;

        let cwnd = self.reno.window() as u16;
        let last_received_ack = self.last_received_ack;
        let local_seq = self.seq;
        let local_ack = self.ack;
        let duplicate_ack_count = self.get_duplicate_ack_count();

        let res = if rcvd_ack > local_seq {
            PacketStatus::Invalid
        } else {
            match self.state {
                TcpState::Listen => PacketStatus::Invalid,
                TcpState::SynReceived => {
                    if has_ack && rcvd_ack == local_seq && rcvd_seq == local_ack {
                        PacketStatus::Ack // expecting a SYN-ACK packet, ensure handshake is complete
                    } else {
                        PacketStatus::Invalid
                    }
                }
                TcpState::Established => {
                    match rcvd_ack.cmp(&last_received_ack) {
                        std::cmp::Ordering::Less => PacketStatus::Invalid, // stale ACK
                        std::cmp::Ordering::Equal => {
                            if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,       // duplicate data
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,    // new data
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder, // unordered data
                                }
                            } else if rcvd_window != cwnd {
                                PacketStatus::WindowUpdate
                            } else if rcvd_seq == local_ack - 1 {
                                PacketStatus::KeepAlive // Keepalive Detection
                            } else if local_seq != last_received_ack && duplicate_ack_count >= MAX_COUNT_FOR_DUP_ACK {
                                PacketStatus::RetransmissionRequest
                            } else {
                                PacketStatus::DuplicateAck // Duplicate ACK
                            }
                        }
                        std::cmp::Ordering::Greater => {
                            if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder,
                                }
                            } else {
                                PacketStatus::Ack
                            }
                        }
                    }
                }
                TcpState::FinWait1 => {
                    match rcvd_ack.cmp(&last_received_ack) {
                        std::cmp::Ordering::Less => PacketStatus::Invalid,
                        std::cmp::Ordering::Equal => {
                            if has_fin && has_ack && payload.is_empty() {
                                PacketStatus::Fin // The other side is closed
                            } else if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder,
                                }
                            } else if rcvd_window != cwnd {
                                PacketStatus::WindowUpdate
                            } else {
                                PacketStatus::DuplicateAck // Duplicate ACK, ignore
                            }
                        }
                        std::cmp::Ordering::Greater => {
                            if has_fin && has_ack && payload.is_empty() {
                                PacketStatus::Fin
                            } else if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder,
                                }
                            } else {
                                PacketStatus::Ack // Confirm our FIN
                            }
                        }
                    }
                }
                TcpState::FinWait2 => {
                    match rcvd_ack.cmp(&last_received_ack) {
                        std::cmp::Ordering::Less => PacketStatus::Invalid,
                        std::cmp::Ordering::Equal => {
                            if has_fin && has_ack && payload.is_empty() {
                                PacketStatus::Fin // The other side is closed
                            } else if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder,
                                }
                            } else if rcvd_window != cwnd {
                                PacketStatus::WindowUpdate
                            } else {
                                PacketStatus::DuplicateAck // Duplicate ACK, ignore
                            }
                        }
                        std::cmp::Ordering::Greater => {
                            if has_fin && has_ack && payload.is_empty() {
                                PacketStatus::Fin
                            } else if !payload.is_empty() {
                                match rcvd_seq.cmp(&local_ack) {
                                    std::cmp::Ordering::Less => PacketStatus::Invalid,
                                    std::cmp::Ordering::Equal => PacketStatus::NewPacket,
                                    std::cmp::Ordering::Greater => PacketStatus::OutOfOrder,
                                }
                            } else {
                                PacketStatus::Ack // Confirm our FIN
                            }
                        }
                    }
                }
                TcpState::CloseWait => {
                    if rcvd_ack <= last_received_ack {
                        if rcvd_window != cwnd {
                            PacketStatus::WindowUpdate
                        } else {
                            PacketStatus::DuplicateAck // Repeat ACK, waiting for us to send FIN
                        }
                    } else if !payload.is_empty() {
                        PacketStatus::Invalid // Ignore new data because we have closed the connection
                    } else {
                        PacketStatus::Ack
                    }
                }
                TcpState::LastAck => {
                    if has_ack && rcvd_ack > last_received_ack && payload.is_empty() {
                        PacketStatus::Ack // Confirm our FIN
                    } else {
                        PacketStatus::Invalid
                    }
                }
                TcpState::TimeWait => {
                    if has_fin && has_ack && rcvd_ack == local_seq && rcvd_seq == local_ack - 1 {
                        PacketStatus::Fin // Retransmitted FIN
                    } else {
                        PacketStatus::Invalid
                    }
                }
                TcpState::Closed => PacketStatus::Invalid,
            }
        };
        #[rustfmt::skip]
        log::trace!("received {{ ack = {rcvd_ack}, seq = {rcvd_seq}, window = {rcvd_window} }}, self {{ ack = {local_ack}, seq = {local_seq}, send_window = {cwnd} }}, {res:?}");
        res
    }

    pub(super) fn add_inflight_packet(&mut self, buf: Vec<u8>) -> std::io::Result<()> {
        let buf_len = buf.len() as u32;
        self.inflight_packets.push_back(InflightPacket::new(self.seq, buf));
        self.seq += buf_len;
        Ok(())
    }

    pub(super) fn update_last_received_ack(&mut self, ack: SeqNum) {
        self.last_received_ack = ack;

        if self.state != TcpState::Established {
            return;
        }
        self.update_inflight_packet_queue(ack);
    }

    pub(crate) fn update_inflight_packet_queue(&mut self, ack: SeqNum) {
        if let Some(index) = self.inflight_packets.iter().position(|p| p.contains_seq_num(ack - 1)) {
            let Some(mut inflight_packet) = self.inflight_packets.remove(index) else {
                log::warn!("Failed to find inflight packet with seq = {}", ack - 1);
                return;
            };
            let distance = ack.distance(inflight_packet.seq) as usize;
            if distance < inflight_packet.payload.len() {
                inflight_packet.payload.drain(0..distance);
                inflight_packet.seq = ack;
                self.inflight_packets.push_back(inflight_packet);
            }
            self.reno.on_ack(distance); // recived ACK, update Reno's cwnd
        }
        self.inflight_packets.retain(|p| ack < p.seq + (p.payload.len() as u32));
    }

    pub(crate) fn find_inflight_packet(&self, seq: SeqNum) -> Option<&InflightPacket> {
        self.inflight_packets.iter().find(|p| p.seq == seq)
    }

    pub(crate) fn get_all_inflight_packets(&self) -> &VecDeque<InflightPacket> {
        &self.inflight_packets
    }

    pub fn is_send_buffer_full(&self) -> bool {
        let effective_window = self.reno.window().min(u16::MAX as usize) as u32;
        (self.seq - self.last_received_ack).0 >= effective_window
    }
}

#[derive(Debug)]
pub struct InflightPacket {
    pub seq: SeqNum,
    pub payload: Vec<u8>,
    // pub send_time: SystemTime, // todo
}

impl InflightPacket {
    fn new(seq: SeqNum, payload: Vec<u8>) -> Self {
        Self {
            seq,
            payload,
            // send_time: SystemTime::now(), // todo
        }
    }
    pub(crate) fn contains_seq_num(&self, seq: SeqNum) -> bool {
        self.seq <= seq && seq < self.seq + self.payload.len() as u32
    }
}

#[test]
fn test_in_flight_packet() {
    let p = InflightPacket::new((u32::MAX - 1).into(), vec![10, 20, 30, 40, 50]);

    assert!(p.contains_seq_num((u32::MAX - 1).into()));
    assert!(p.contains_seq_num(u32::MAX.into()));
    assert!(p.contains_seq_num(0.into()));
    assert!(p.contains_seq_num(1.into()));
    assert!(p.contains_seq_num(2.into()));

    assert!(!p.contains_seq_num(3.into()));
}

#[derive(Debug)]
struct UnorderedPacket {
    payload: Vec<u8>,
    // pub recv_time: SystemTime, // todo
}

impl UnorderedPacket {
    pub(crate) fn new(payload: Vec<u8>) -> Self {
        Self {
            payload,
            // recv_time: SystemTime::now(), // todo
        }
    }
}
