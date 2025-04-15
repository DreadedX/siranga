use std::sync::atomic::{AtomicUsize, Ordering};

use crate::units::Unit;

#[derive(Debug, Default)]
pub struct Stats {
    connections: AtomicUsize,
    rx: AtomicUsize,
    tx: AtomicUsize,
}

impl Stats {
    pub fn add_connection(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_rx_bytes(&self, n: usize) {
        self.rx.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_tx_bytes(&self, n: usize) {
        self.tx.fetch_add(n, Ordering::Relaxed);
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn rx(&self) -> Unit {
        Unit::new(self.rx.load(Ordering::Relaxed), "B")
    }

    pub fn tx(&self) -> Unit {
        Unit::new(self.tx.load(Ordering::Relaxed), "B")
    }
}
