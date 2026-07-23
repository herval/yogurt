//! Mic-clocked two-stream mixer.
//!
//! Output cadence follows the microphone stream (its delegate delivers
//! continuously); system-tap audio is summed in as available. The tap backlog
//! is bounded so rate drift can't grow latency without limit.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::Duration;

/// Max buffered system audio: 2 seconds at 16kHz.
const MAX_SYS_BACKLOG: usize = 32000;

pub fn spawn_mixer(
    mic_rx: Receiver<Vec<u8>>,
    sys_rx: Receiver<Vec<u8>>,
    out: SyncSender<Vec<u8>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut sys_q: VecDeque<i16> = VecDeque::new();
        loop {
            let drain_sys = |sys_q: &mut VecDeque<i16>| {
                loop {
                    match sys_rx.try_recv() {
                        Ok(chunk) => {
                            for c in chunk.chunks_exact(2) {
                                sys_q.push_back(i16::from_le_bytes([c[0], c[1]]));
                            }
                        }
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
                if sys_q.len() > MAX_SYS_BACKLOG {
                    let excess = sys_q.len() - MAX_SYS_BACKLOG;
                    sys_q.drain(..excess);
                }
            };

            match mic_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(mic) => {
                    drain_sys(&mut sys_q);
                    let mut mixed = Vec::with_capacity(mic.len());
                    for c in mic.chunks_exact(2) {
                        let m = i16::from_le_bytes([c[0], c[1]]) as i32;
                        let s = sys_q.pop_front().unwrap_or(0) as i32;
                        let sum = (m + s).clamp(-32768, 32767) as i16;
                        mixed.extend_from_slice(&sum.to_le_bytes());
                    }
                    if out.send(mixed).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Mic paused/quiet spell: keep the tap queue fresh & bounded.
                    drain_sys(&mut sys_q);
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })
}

/// Mic-clocked stereo interleaver: left = mic, right = system audio.
/// Gives STT providers channel-separated speakers ("you" vs "them").
pub fn spawn_interleaver(
    mic_rx: Receiver<Vec<u8>>,
    sys_rx: Receiver<Vec<u8>>,
    out: SyncSender<Vec<u8>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut sys_q: VecDeque<i16> = VecDeque::new();
        loop {
            let drain_sys = |sys_q: &mut VecDeque<i16>| {
                loop {
                    match sys_rx.try_recv() {
                        Ok(chunk) => {
                            for c in chunk.chunks_exact(2) {
                                sys_q.push_back(i16::from_le_bytes([c[0], c[1]]));
                            }
                        }
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
                if sys_q.len() > MAX_SYS_BACKLOG {
                    let excess = sys_q.len() - MAX_SYS_BACKLOG;
                    sys_q.drain(..excess);
                }
            };

            match mic_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(mic) => {
                    drain_sys(&mut sys_q);
                    let mut stereo = Vec::with_capacity(mic.len() * 2);
                    for c in mic.chunks_exact(2) {
                        stereo.extend_from_slice(c); // L: mic
                        let s = sys_q.pop_front().unwrap_or(0);
                        stereo.extend_from_slice(&s.to_le_bytes()); // R: system
                    }
                    if out.send(stereo).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    drain_sys(&mut sys_q);
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })
}
