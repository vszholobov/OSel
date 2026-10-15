//! Synchronous station setup; the connected worker uses the same event pump for rekeys.
use super::*;
use osel_core::wifi::{station, wpa2};

pub(super) fn nonce(output: &mut [u8; 32]) -> KResult<()> {
    // Runtime CPUID gate before an optional instruction; no PIT/getrandom fallback.
    if core::arch::x86_64::__cpuid(1).ecx & (1 << 30) == 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    station::nonce(
        || {
            let mut word = 0;
            if unsafe { core::arch::x86_64::_rdrand64_step(&mut word) } == 1 {
                Some(word)
            } else {
                None
            }
        },
        output,
    )
}
impl Controller {
    fn station_wmi(&mut self, event: Vec<u8>, report: &mut Report) -> KResult<()> {
        if event.len() < 4 {
            return Err(Errno::EIO);
        }
        let id = u32::from_le_bytes(event[..4].try_into().unwrap()) & 0xffffff;
        if wmi::station::lost(&event, self.data.as_ref().ok_or(Errno::ENODEV)?.ap)? {
            report.record("WMI AP connection lost", id);
            return Err(Errno::ENETUNREACH);
        }
        if id == 0x5001 {
            wmi::station::started(&event)?;
            self.data.as_mut().ok_or(Errno::ENODEV)?.started = true;
        }
        if id == 0x7001 {
            let frame = match wmi::management(&event) {
                Ok(frame) => frame,
                Err(_) => return Ok(()),
            };
            let data = self.data.as_mut().ok_or(Errno::ENODEV)?;
            if frame.frequency != data.frequency {
                return Ok(());
            }
            match station::response(frame.frame, data.station, data.ap)? {
                station::Response::Disconnected(reason) => {
                    report.record("AP disconnected reason", u32::from(reason));
                    data.authorized = false;
                    return Err(Errno::ENETUNREACH);
                }
                station::Response::Authenticated | station::Response::Associated(_) => {
                    if data.management.len() >= 8 {
                        return Err(Errno::EAGAIN);
                    }
                    data.management.push_back(frame.frame.to_vec());
                }
                station::Response::Other => (),
            }
        }
        Ok(())
    }
    pub(super) fn station_poll(&mut self, report: &mut Report) -> KResult<()> {
        self.check_operation()?;
        let indicator = self.get(INDICATOR);
        if indicator == u32::MAX || indicator & 1 != 0 {
            report.record("firmware target crashed", indicator);
            return Err(Errno::EIO);
        }
        for _ in 0..64 {
            if let Some(event) = self.htt_events.pop_front() {
                self.handle_htt(&event, report)?;
                continue;
            }
            if let Some(event) = self.wmi_events.pop_front() {
                self.station_wmi(event, report)?;
                continue;
            }
            if self.descriptor_length(6, self.wmi_rx) == 0
                && self.descriptor_length(1, self.rx) == 0
            {
                break;
            }
            if let Some((endpoint, event)) = self.receive_event(1, report)? {
                if endpoint == self.htt_endpoint {
                    self.handle_htt(&event, report)?;
                } else {
                    self.station_wmi(event, report)?;
                }
            }
        }
        if let Some(id) = self
            .data
            .as_ref()
            .and_then(|data| data.expired_tx(time::monotonic_ns()))
        {
            report.record("HTT TX completion timeout", id as u32);
            return Err(Errno::ETIMEDOUT);
        }
        Ok(())
    }
    fn wait_station(
        &mut self,
        timeout: u64,
        report: &mut Report,
        mut ready: impl FnMut(&mut data::Data) -> KResult<bool>,
    ) -> KResult<()> {
        let end = time::monotonic_ns().saturating_add(timeout * 1_000_000);
        loop {
            self.station_poll(report)?;
            if ready(self.data.as_mut().ok_or(Errno::ENODEV)?)? {
                return Ok(());
            }
            if time::monotonic_ns() >= end {
                return Err(Errno::ETIMEDOUT);
            }
            delay(10)?;
        }
    }
    fn frame_and_wait(
        &mut self,
        frame: &[u8],
        management: bool,
        encrypt: bool,
        report: &mut Report,
    ) -> KResult<()> {
        let id = self.transmit_frame(frame, management, encrypt, report)?;
        self.wait_station(3000, report, |data| match data.completion(id) {
            Some(0) => Ok(true),
            Some(_) => Err(Errno::EIO),
            None => Ok(false),
        })
    }
    pub(super) fn associate(
        &mut self,
        ap: &AccessPoint,
        pmk: wpa2::Pmk,
        report: &mut Report,
    ) -> KResult<wpa2::Handshake> {
        station::supported(ap)?;
        if (ap.frequency as u32) < self.channel_limits[0]
            || ap.frequency as u32 > self.channel_limits[1]
        {
            return Err(Errno::EOPNOTSUPP);
        }
        let data = self.data.as_mut().ok_or(Errno::ENODEV)?;
        data.ap = ap.bssid;
        data.frequency = ap.frequency;
        self.send_wmi(self.wmi_endpoint, &wmi::vdev_create(self.mac)?, report)?;
        self.send_wmi(self.wmi_endpoint, &wmi::station::vdev_param(48, 2)?, report)?; // Ethernet TX
        report.record("STA vdev start channel", u32::from(ap.frequency));
        self.send_wmi(self.wmi_endpoint, &wmi::station::start(ap)?, report)?;
        self.wait_station(3000, report, |data| Ok(data.started))?;
        self.send_wmi(
            self.wmi_endpoint,
            &wmi::station::create_peer(ap.bssid)?,
            report,
        )?;
        self.wait_station(3000, report, |data| Ok(data.peer.is_some()))?;
        report.record("STA open authentication begin", 0);
        self.frame_and_wait(
            &station::authenticate(self.mac, ap.bssid, 0),
            true,
            false,
            report,
        )?;
        let mac = self.mac;
        self.wait_station(3000, report, |data| {
            while let Some(frame) = data.management.pop_front() {
                if matches!(
                    station::response(&frame, mac, ap.bssid)?,
                    station::Response::Authenticated
                ) {
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        report.record("STA association begin", 0);
        self.frame_and_wait(&station::associate(self.mac, ap, 1)?, true, false, report)?;
        let mut aid = 0;
        self.wait_station(3000, report, |data| {
            while let Some(frame) = data.management.pop_front() {
                if let station::Response::Associated(value) =
                    station::response(&frame, mac, ap.bssid)?
                {
                    aid = value;
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        report.record("STA association AID", u32::from(aid));
        self.send_wmi(
            self.wmi_endpoint,
            &wmi::station::associated(ap, aid)?,
            report,
        )?;
        self.send_wmi(self.wmi_endpoint, &wmi::station::up(ap.bssid, aid)?, report)?;
        self.send_wmi(
            self.wmi_endpoint,
            &wmi::station::peer_param(ap.bssid, 0xff, 1)?,
            report,
        )?; // QCA6174 powersave workaround
        let mut handshake = wpa2::Handshake::new(pmk, self.mac, ap.bssid)?;
        report.record("WPA2 EAPOL handshake wait", 0);
        let deadline = time::monotonic_ns().saturating_add(15_000_000_000);
        loop {
            self.station_poll(report)?;
            self.eapol(&mut handshake, report)?;
            if handshake.authorized() {
                report.record("WPA2 keys installed; port authorized", 1);
                return Ok(handshake);
            }
            if time::monotonic_ns() >= deadline {
                return Err(Errno::ETIMEDOUT);
            }
            delay(10)?;
        }
    }
    fn install_key(
        &mut self,
        ap: [u8; 6],
        group: bool,
        index: u8,
        rsc: [u8; 8],
        key: &[u8; 16],
        report: &mut Report,
    ) -> KResult<()> {
        let counter = usize::from(!group);
        let previous = self.data.as_ref().ok_or(Errno::ENODEV)?.security[counter];
        let command = wmi::station::key(ap, group, index, rsc, key)?;
        self.send_wmi(self.wmi_endpoint, &command, report)?;
        // This firmware acknowledges installation with HTT SEC_IND, like ath10k.
        self.wait_station(3000, report, |data| Ok(data.security[counter] > previous))?;
        if group {
            self.data
                .as_mut()
                .unwrap()
                .replay
                .install_group(index, rsc)?;
        } else {
            self.data.as_mut().unwrap().replay.reset_pairwise();
        }
        Ok(())
    }
    pub(super) fn eapol(
        &mut self,
        handshake: &mut wpa2::Handshake,
        report: &mut Report,
    ) -> KResult<()> {
        for _ in 0..64 {
            let data = self.data.as_mut().ok_or(Errno::ENODEV)?;
            let Some(index) = data
                .received
                .iter()
                .position(|frame| frame[12..14] == [0x88, 0x8e])
            else {
                break;
            };
            let frame = data.received.remove(index).unwrap();
            let ap = data.ap;
            let action = match handshake.receive(&frame[14..], nonce) {
                Ok(action) => action,
                // Bad MIC/replay/key-data from the air never installs keys or tears
                // down a good connection. A missing valid M3 reaches the deadline.
                Err(Errno::EACCES | Errno::EIO | Errno::EALREADY | Errno::EOPNOTSUPP) => {
                    data.drops += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let (reply, installed) = match action {
                wpa2::Action::Reply(reply) => (reply, false),
                wpa2::Action::Install { keys, reply } => {
                    if let Some(key) = &keys.pairwise {
                        self.install_key(ap, false, 0, [0; 8], key, report)?;
                    }
                    if let Some(key) = &keys.group {
                        self.install_key(ap, true, key.index, key.rsc, &key.key, report)?;
                    }
                    (reply, true)
                }
            };
            let mut ethernet = ap.to_vec();
            ethernet.extend_from_slice(&self.mac);
            ethernet.extend_from_slice(&[0x88, 0x8e]);
            ethernet.extend_from_slice(&reply);
            self.frame_and_wait(&ethernet, false, false, report)?;
            if installed {
                handshake.installed()?;
                self.send_wmi(
                    self.wmi_endpoint,
                    &wmi::station::peer_param(ap, 3, 1)?,
                    report,
                )?;
                self.data.as_mut().unwrap().authorized = true;
            }
        }
        Ok(())
    }
}
