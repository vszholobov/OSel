//! Bounded Ethernet queues between smoltcp and the firmware worker.
//! Lock order: NETWORK -> WIFI_LINK. The worker releases WIFI_LINK before I/O.
use super::*;
use smoltcp::{
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
};
static LINK: IrqSpinlock<Option<Link>> = IrqSpinlock::new("WIFI_LINK", None);
struct Link {
    generation: u64,
    mac: [u8; 6],
    up: bool,
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
    counters: (u64, u64, u64),
}
#[derive(Clone, Copy)]
pub struct Wireless {
    generation: u64,
    mac: [u8; 6],
}
impl Wireless {
    pub(crate) fn new(mac: [u8; 6]) -> Self {
        let mut link = LINK.lock();
        let generation = link
            .as_ref()
            .map_or(1, |link| link.generation.wrapping_add(1));
        *link = Some(Link {
            generation,
            mac,
            up: true,
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            counters: (0, 0, 0),
        });
        Self { generation, mac }
    }
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }
    pub fn link(&self) -> bool {
        LINK.lock().as_ref().is_some_and(|link| {
            link.generation == self.generation && link.mac == self.mac && link.up
        })
    }
    pub fn counters(&self) -> (u64, u64, u64) {
        LINK.lock()
            .as_ref()
            .filter(|link| link.generation == self.generation)
            .map_or((0, 0, 0), |link| link.counters)
    }
    pub(crate) fn down(&self) {
        let mut link = LINK.lock();
        if let Some(link) = link
            .as_mut()
            .filter(|link| link.generation == self.generation)
        {
            link.up = false;
            link.rx = VecDeque::new();
            link.tx = VecDeque::new();
        }
    }
    pub(super) fn dropped(&self, count: u64) {
        if let Some(link) = LINK
            .lock()
            .as_mut()
            .filter(|link| link.generation == self.generation)
        {
            link.counters.2 += count;
        }
    }
    pub(super) fn take_tx(&self) -> Option<Vec<u8>> {
        LINK.lock()
            .as_mut()
            .filter(|link| link.generation == self.generation && link.up)?
            .tx
            .pop_front()
    }
    pub(super) fn received(&self, frame: Vec<u8>) {
        let mut link = LINK.lock();
        if let Some(link) = link
            .as_mut()
            .filter(|link| link.generation == self.generation && link.up)
        {
            if frame.len() > 1514 || link.rx.len() >= 64 {
                link.counters.2 += 1;
            } else {
                link.rx.push_back(frame);
                link.counters.0 += 1;
            }
        }
    }
}
pub(super) fn status(out: &mut impl Write) {
    let snapshot = LINK.lock().as_ref().map(|link| {
        (
            link.up,
            link.generation,
            link.counters,
            link.rx.len(),
            link.tx.len(),
        )
    });
    if let Some((up, generation, (rx, tx, dropped), pending_rx, pending_tx)) = snapshot {
        let _ = writeln!(
            out,
            "WiFi link={up} generation={generation} rx={rx} tx={tx} dropped={dropped} pending_rx={pending_rx} pending_tx={pending_tx}"
        );
    }
}
pub struct Receive(Vec<u8>);
impl RxToken for Receive {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
pub struct Transmit(Wireless);
impl TxToken for Transmit {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = alloc::vec![0;len];
        let result = f(&mut frame);
        let mut link = LINK.lock();
        if let Some(link) = link
            .as_mut()
            .filter(|link| link.generation == self.0.generation && link.up)
        {
            if !(14..=1514).contains(&len) || link.tx.len() >= 64 {
                link.counters.2 += 1;
            } else {
                link.tx.push_back(frame);
                link.counters.1 += 1;
            }
        }
        result
    }
}
impl Device for Wireless {
    type RxToken<'a> = Receive;
    type TxToken<'a> = Transmit;
    fn receive(&mut self, _: Instant) -> Option<(Receive, Transmit)> {
        let mut link = LINK.lock();
        let link = link
            .as_mut()
            .filter(|link| link.generation == self.generation && link.up && link.tx.len() < 64)?;
        Some((Receive(link.rx.pop_front()?), Transmit(*self)))
    }
    fn transmit(&mut self, _: Instant) -> Option<Transmit> {
        LINK.lock()
            .as_ref()
            .filter(|link| link.generation == self.generation && link.up && link.tx.len() < 64)?;
        Some(Transmit(*self))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(8);
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test_case]
    fn bridge_bounds_queues_and_revokes_tokens_from_a_previous_session() {
        let old = Wireless::new([2; 6]);
        let mut adapter = old;
        let stale = adapter.transmit(Instant::from_millis(0)).unwrap();
        for _ in 0..64 {
            adapter
                .transmit(Instant::from_millis(0))
                .unwrap()
                .consume(60, |frame| frame.fill(7));
        }
        assert!(adapter.transmit(Instant::from_millis(0)).is_none());
        assert_eq!(old.take_tx().unwrap(), alloc::vec![7;60]);
        let next = Wireless::new([4; 6]);
        stale.consume(60, |_| ());
        assert!(next.take_tx().is_none());
        assert!(!old.link());
        next.down();
        assert!(!next.link());
        *LINK.lock() = None;
    }
}
